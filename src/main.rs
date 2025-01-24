//! An example utilizing the `EspWifiNCMatterStack` struct.
//!
//! As the name suggests, this Matter stack assembly uses Wifi as the main transport,
//! (and thus BLE for commissioning), where `NC` stands for non-concurrent commisisoning
//! (i.e., the stack will not run the BLE and Wifi radio simultaneously, which saves memory).
//!
//! If you want to use Ethernet, utilize `EspEthMatterStack` instead.
//! If you want to use concurrent commissioning, utilize `EspWifiMatterStack` instead
//! (Alexa does not work (yet) with non-concurrent commissioning).
//!
//! The example implements a fictitious Light device (an On-Off Matter cluster).

use core::pin::pin;
use core::time::Duration;

use embassy_futures::select::select;
use embassy_time::Timer;

use esp_idf_hal::delay::{TickType, BLOCK};
use esp_idf_hal::units::KiloHertz;
use esp_idf_matter::matter::data_model::cluster_basic_information::BasicInfoConfig;
use esp_idf_matter::matter::data_model::cluster_on_off;
use esp_idf_matter::matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use esp_idf_matter::matter::data_model::objects::{Dataver, Endpoint, HandlerCompat, Node};
use esp_idf_matter::matter::data_model::system_model::descriptor;
use esp_idf_matter::matter::utils::init::InitMaybeUninit;
use esp_idf_matter::matter::utils::select::Coalesce;
use esp_idf_matter::persist;
use esp_idf_matter::stack::test_device::{TEST_BASIC_COMM_DATA, TEST_DEV_ATT, TEST_PID, TEST_VID};
use esp_idf_matter::{init_async_io, EspMatterBle, EspMatterWifi, EspWifiNCMatterStack};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;

use log::{error, info};

use rs_matter::data_model::objects::DeviceType;
use static_cell::StaticCell;

mod humidity_measurement;
mod temperature_measurement;

fn main() -> Result<(), anyhow::Error> {
    EspLogger::initialize_default();

    info!("Starting...");

    // Run in a higher-prio thread to avoid issues with `async-io` getting
    // confused by the low priority of the ESP IDF main task
    // Also allocate a very large stack (for now) as `rs-matter` futures do occupy quite some space
    let thread = std::thread::Builder::new()
        .stack_size(120 * 1024)
        .spawn(|| {
            // Eagerly initialize `async-io` to minimize the risk of stack blowups later on
            init_async_io()?;

            run()
        })
        .unwrap();

    thread.join().unwrap()
}

#[inline(never)]
#[cold]
fn run() -> Result<(), anyhow::Error> {
    let result = block_on(matter());

    if let Err(e) = &result {
        error!("Matter aborted execution with error: {:?}", e);
    }
    {
        info!("Matter finished execution successfully");
    }

    result
}

async fn matter() -> Result<(), anyhow::Error> {
    // Initialize the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK
        .uninit()
        .init_with(EspWifiNCMatterStack::init_default(
            &BasicInfoConfig {
                vid: TEST_VID,
                pid: TEST_PID,
                hw_ver: 2,
                sw_ver: 1,
                sw_ver_str: "1",
                serial_no: "aabbccdd",
                device_name: "MyLight",
                product_name: "ACME Light",
                vendor_name: "ACME",
            },
            TEST_BASIC_COMM_DATA,
            &TEST_DEV_ATT,
        ));

    // Take some generic ESP-IDF stuff we'll need later
    let sysloop = EspSystemEventLoop::take()?;
    let timers = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let peripherals = Peripherals::take()?;

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = cluster_on_off::OnOffCluster::new(Dataver::new_rand(stack.matter().rand()));

    let temperature_measurement = temperature_measurement::TemperatureMeasurementCluster::new(
        Dataver::new_rand(stack.matter().rand()),
    );
    let humidity_measurement = humidity_measurement::HumidityMeasurementCluster::new(
        Dataver::new_rand(stack.matter().rand()),
    );

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = stack
        .root_handler()
        // Our on-off cluster, on Endpoint 1
        .chain(
            LIGHT_ENDPOINT_ID,
            cluster_on_off::ID,
            HandlerCompat(&on_off),
        )
        .chain(
            TEMPERATURE_SENSOR_ENDPOINT_ID,
            temperature_measurement::ID,
            HandlerCompat(&temperature_measurement),
        )
        .chain(
            HUMIDITY_SENSOR_ENDPOINT_ID,
            humidity_measurement::ID,
            HandlerCompat(&humidity_measurement),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            LIGHT_ENDPOINT_ID,
            descriptor::ID,
            HandlerCompat(descriptor::DescriptorCluster::new(Dataver::new_rand(
                stack.matter().rand(),
            ))),
        )
        .chain(
            TEMPERATURE_SENSOR_ENDPOINT_ID,
            descriptor::ID,
            HandlerCompat(descriptor::DescriptorCluster::new(Dataver::new_rand(
                stack.matter().rand(),
            ))),
        )
        .chain(
            HUMIDITY_SENSOR_ENDPOINT_ID,
            descriptor::ID,
            HandlerCompat(descriptor::DescriptorCluster::new(Dataver::new_rand(
                stack.matter().rand(),
            ))),
        );

    let (mut wifi_modem, mut bt_modem) = peripherals.modem.split();

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let mut matter = pin!(stack.run(
        // The Matter stack needs the Wifi modem peripheral
        EspMatterWifi::new(&mut wifi_modem, sysloop, timers, nvs.clone()),
        // The Matter stack needs the BT modem peripheral
        EspMatterBle::new(&mut bt_modem, nvs.clone(), stack),
        // The Matter stack needs a persister to store its state
        // `EspPersist`+`EspKvBlobStore` saves to a user-supplied NVS partition
        // under namespace `esp-idf-matter`
        persist::new_default(nvs, stack)?,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user future to run
        core::future::pending(),
    ));

    let mut device = pin!(async {
        let mut switch = esp_idf_hal::gpio::PinDriver::input(peripherals.pins.gpio41).unwrap();
        switch.set_pull(esp_idf_hal::gpio::Pull::Up).unwrap();

        let i2c = peripherals.i2c0;
        let sda = peripherals.pins.gpio2;
        let scl = peripherals.pins.gpio1;
        let config = esp_idf_hal::i2c::I2cConfig::new()
            .baudrate(KiloHertz::from(100).into())
            .scl_enable_pullup(true)
            .sda_enable_pullup(true);
        let mut i2c = esp_idf_hal::i2c::I2cDriver::new(i2c, sda, scl, &config).unwrap();
        const SHT40_ADDRESS: u8 = 0x44;

        i2c.write(SHT40_ADDRESS, &[0x94], BLOCK).unwrap();

        let led = peripherals.pins.gpio35;
        let channel = peripherals.rmt.channel0;
        let config = esp_idf_hal::rmt::config::TransmitConfig::new().clock_divider(1);
        let mut tx = esp_idf_hal::rmt::TxRmtDriver::new(channel, led, &config).unwrap();
        let mut last_switch = switch.is_low();
        loop {
            if let Ok(_) = i2c.write(SHT40_ADDRESS, &[0xFD], TickType::new_millis(100).ticks()) {
                Timer::after(embassy_time::Duration::from_millis(10)).await;
                let mut buffer = [0u8; 6];
                if let Ok(_) = i2c.read(
                    SHT40_ADDRESS,
                    &mut buffer,
                    TickType::new_millis(100).ticks(),
                ) {
                    let temperature = ((buffer[0] as u16) << 8 | buffer[1] as u16) as f32 * 175.0
                        / 65535.0
                        - 45.0;
                    let relative_humidity =
                        (((buffer[3] as u16) << 8 | buffer[4] as u16) as f32 * 125.0 / 65535.0
                            - 6.0)
                            .clamp(0.0, 100.0);
                    //log::info!("Temperature: {:.2}°C", temperature);
                    //log::info!("Relative Humidity: {:.2}%", relative_humidity);
                    temperature_measurement.set(Some(temperature));
                    humidity_measurement.set(Some(relative_humidity));
                }
            }
            let switch_pressed = switch.is_low();
            if switch_pressed && !last_switch {
                on_off.set(!on_off.get());
                stack.notify_changed();
            }
            last_switch = switch_pressed;

            if on_off.get() {
                neopixel(0xffffff, &mut tx).unwrap();
            } else {
                neopixel(0x000000, &mut tx).unwrap();
            }
            Timer::after(embassy_time::Duration::from_millis(100)).await;
        }
    });

    // Schedule the Matter run & the device loop together
    select(&mut matter, &mut device).coalesce().await?;

    Ok(())
}

fn neopixel(color: u32, tx: &mut esp_idf_hal::rmt::TxRmtDriver) -> anyhow::Result<()> {
    let ticks_hz = tx.counter_clock()?;
    let (t0h, t0l, t1h, t1l) = (
        esp_idf_hal::rmt::Pulse::new_with_duration(
            ticks_hz,
            esp_idf_hal::rmt::PinState::High,
            &Duration::from_nanos(350),
        )?,
        esp_idf_hal::rmt::Pulse::new_with_duration(
            ticks_hz,
            esp_idf_hal::rmt::PinState::Low,
            &Duration::from_nanos(800),
        )?,
        esp_idf_hal::rmt::Pulse::new_with_duration(
            ticks_hz,
            esp_idf_hal::rmt::PinState::High,
            &Duration::from_nanos(700),
        )?,
        esp_idf_hal::rmt::Pulse::new_with_duration(
            ticks_hz,
            esp_idf_hal::rmt::PinState::Low,
            &Duration::from_nanos(600),
        )?,
    );
    let mut signal = esp_idf_hal::rmt::FixedLengthSignal::<24>::new();
    for i in (0..24).rev() {
        let p = 2_u32.pow(i);
        let bit: bool = p & color != 0;
        let (high_pulse, low_pulse) = if bit { (t1h, t1l) } else { (t0h, t0l) };
        signal.set(23 - i as usize, &(high_pulse, low_pulse))?;
    }
    tx.start_blocking(&signal)?;
    Ok(())
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
/// It is also a mandatory requirement when the `WifiBle` stack variation is used.
static MATTER_STACK: StaticCell<EspWifiNCMatterStack<()>> = StaticCell::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;
const TEMPERATURE_SENSOR_ENDPOINT_ID: u16 = 2;
const HUMIDITY_SENSOR_ENDPOINT_ID: u16 = 3;

pub const DEV_TYPE_TEMPERATURE_SENSOR: DeviceType = DeviceType {
    dtype: 0x0302,
    drev: 2,
};
pub const DEV_TYPE_HUMIDITY_SENSOR: DeviceType = DeviceType {
    dtype: 0x0307,
    drev: 2,
};

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EspWifiNCMatterStack::<()>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: &[DEV_TYPE_ON_OFF_LIGHT],
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
        Endpoint {
            id: TEMPERATURE_SENSOR_ENDPOINT_ID,
            device_types: &[DEV_TYPE_TEMPERATURE_SENSOR],
            clusters: &[descriptor::CLUSTER, temperature_measurement::CLUSTER],
        },
        Endpoint {
            id: HUMIDITY_SENSOR_ENDPOINT_ID,
            device_types: &[DEV_TYPE_HUMIDITY_SENSOR],
            clusters: &[descriptor::CLUSTER, humidity_measurement::CLUSTER],
        },
    ],
};

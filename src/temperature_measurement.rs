/*
*
*    Copyright (c) 2020-2022 Project CHIP Authors
*
*    Licensed under the Apache License, Version 2.0 (the "License");
*    you may not use this file except in compliance with the License.
*    You may obtain a copy of the License at
*
*        http://www.apache.org/licenses/LICENSE-2.0
*
*    Unless required by applicable law or agreed to in writing, software
*    distributed under the License is distributed on an "AS IS" BASIS,
*    WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
*    See the License for the specific language governing permissions and
*    limitations under the License.
*/

use std::cell::Cell;

use rs_matter::attribute_enum;
use rs_matter::data_model::objects::{
    Access, AttrType, Attribute, Cluster, Handler, Quality
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::exchange::Exchange;

use rs_matter::data_model::objects::{
    AttrDataEncoder, AttrDetails, ChangeNotifier, Dataver, NonBlockingHandler, ATTRIBUTE_LIST,
    FEATURE_MAP,
};

use strum::{EnumDiscriminants, FromRepr};

pub const ID: u32 = 0x0402;
#[derive(FromRepr, EnumDiscriminants)]
#[repr(u16)]
pub enum Attributes {
    MeasuredValue(AttrType<Option<i16>>) = 0x0,
    MinMeasuredValue(AttrType<Option<i16>>) = 0x1,
    MaxMeasuredValue(AttrType<Option<i16>>) = 0x2,
}

attribute_enum!(Attributes);

pub const MEASURED_VAUE: Attribute = Attribute::new(
    AttributesDiscriminants::MeasuredValue as _,
    Access::RV,
    Quality::from_bits(Quality::NULLABLE.bits() | Quality::PERSISTENT.bits()).unwrap(),
);
pub const MIN_MEASURED_VAUE: Attribute = Attribute::new(
    AttributesDiscriminants::MinMeasuredValue as _,
    Access::RV,
    Quality::X,
);
pub const MAX_MEASURED_VAUE: Attribute = Attribute::new(
    AttributesDiscriminants::MaxMeasuredValue as _,
    Access::RV,
    Quality::X,
);

pub const CLUSTER: Cluster<'static> = Cluster {
    id: ID as _,
    feature_map: 0,
    attributes: &[FEATURE_MAP, ATTRIBUTE_LIST, MEASURED_VAUE, MIN_MEASURED_VAUE, MAX_MEASURED_VAUE],
    commands: &[],
};

pub struct TemperatureMeasurementCluster {
    data_ver: Dataver,
    temperature: Cell<Option<f32>>,
}

impl TemperatureMeasurementCluster {
    pub const fn new(data_ver: Dataver) -> Self {
        Self { data_ver, temperature: Cell::new(None) }
    }

    pub fn get(&self) -> Option<f32> {
        self.temperature.get()
    }

    pub fn set(&self, temperature: Option<f32>) {
        if self.temperature.get() != temperature {
            self.temperature.set(temperature);
            self.data_ver.changed();
        }
    }

    pub fn read(
        &self,
        _exchange: &Exchange,
        attr: &AttrDetails,
        encoder: AttrDataEncoder,
    ) -> Result<(), Error> {
        if let Some(writer) = encoder.with_dataver(self.data_ver.get())? {
            if attr.is_system() {
                CLUSTER.read(attr.attr_id, writer)
            } else {
                match attr.attr_id.try_into()? {
                    Attributes::MeasuredValue(codec) => codec.encode(writer, self.temperature.get().map(|v| (v * 100.0).round() as i16)),
                    Attributes::MinMeasuredValue(codec) => codec.encode(writer, None),
                    Attributes::MaxMeasuredValue(codec) => codec.encode(writer, None),
                }
            }
        } else {
            Ok(())
        }
    }
}

impl Handler for TemperatureMeasurementCluster {
    fn read(
        &self,
        exchange: &Exchange,
        attr: &AttrDetails,
        encoder: AttrDataEncoder,
    ) -> Result<(), Error> {
        TemperatureMeasurementCluster::read(self, exchange, attr, encoder)
    }
}

impl NonBlockingHandler for TemperatureMeasurementCluster {}

impl ChangeNotifier<()> for TemperatureMeasurementCluster {
    fn consume_change(&mut self) -> Option<()> {
        self.data_ver.consume_change(())
    }
}

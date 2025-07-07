//! Device report types and helper functions

use std::collections::HashMap;

use serde::Serialize;

use super::models::Device;

#[derive(Serialize, Debug, Clone)]
struct DeviceReport {}

impl From<Device> for DeviceReport {
    fn from(_: Device) -> Self {
        // TODO
        DeviceReport {}
    }
}

// The state for report
#[derive(Serialize, Debug, Clone)]
pub struct Report(HashMap<String, DeviceReport>);

impl From<Device> for Report {
    fn from(device: Device) -> Self {
        Report(HashMap::from([(device.uuid.clone(), device.into())]))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::models::Device;

    #[test]
    fn it_creates_a_device_report_from_a_device() {
        let device = Device {
            uuid: "test-uuid".to_string(),
            images: HashMap::new(),
        };

        let report: Report = device.into();

        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value, json!({"test-uuid": {}}))
    }
}

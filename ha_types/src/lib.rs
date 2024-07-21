use serde::{Serialize, Deserialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct HAEntity {
    pub name: String,
    pub variant: HAEntityVariant,
    pub unique_id: String,
    pub state_topic: String,
    pub icon: Option<String>,
    #[serde(skip_deserializing)]
    pub availability: Option<HADeviceAvailability>,
    pub device: Option<HADevice>,
    pub device_class: Option<String>,
    pub entity_category: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum HAEntityVariant {
    binary_sensor,
    sensor,
}
impl std::fmt::Display for HAEntityVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HAEntityVariant::binary_sensor => write!(f, "binary_sensor"),
            HAEntityVariant::sensor => write!(f, "sensor"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HADeviceAvailability {
    pub payload_available: Option<String>,
    pub payload_not_available: Option<String>,
    pub topic: String,
    pub value_template: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HADevice {
    pub configuration_url: Option<String>,
    pub hw_version: Option<String>,
    pub identifiers: Option<Vec<String>>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub serial_number: Option<String>,
    pub suggested_area: Option<String>,
    pub sw_version: Option<String>,
    pub via_device: Option<String>,
}


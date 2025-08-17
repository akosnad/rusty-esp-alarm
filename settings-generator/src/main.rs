use clap::Parser as _;
use embedded_storage_file::{NorMemoryAsync, NorMemoryInFile};
use ha_types::HAEntity;
use rusty_esp_alarm::{self as lib};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Reads key value pairs from stdin and convertss them to a flashable settings partition binary
#[derive(clap::Parser)]
struct Args {
    /// Size of partition in bytes
    #[arg(short, long, default_value_t = 0x2000)]
    size: u32,
    /// Input file path (supported file formats: YAML)
    conf_path: String,
    /// Output partition binary path
    bin_path: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let Args {
        size,
        conf_path,
        bin_path,
    } = Args::parse();
    let range = 0..size;

    // READ_SIZE, WRITE_SIZE and ERASE_SIZE carefully chosen to be
    // same as esp_hal::FlashStorage's implementation
    let nor = NorMemoryInFile::<4, 4, 4096>::new(bin_path, size as usize)?;
    let storage = NorMemoryAsync::new(nor);

    let mut buf = [0u8; 4096];
    let settings = lib::settings::Settings::uninit(storage, range, &mut buf);
    let mut settings = settings
        .reset()
        .await
        .map_err(|e| anyhow::anyhow!("settings reset failed: {e:?}"))?;

    let conf_raw = tokio::fs::read(conf_path).await?;
    let configuration: Configuration = serde_yaml::from_slice(&conf_raw)?;

    let mac_address: MACAddress = configuration.mac_address.parse()?;
    settings
        .set("mac-address", &mac_address.addr())
        .await
        .map_err(|e| anyhow::anyhow!("setting mac-address failed: {e:?}"))?;
    settings
        .set("hostname", &configuration.hostname.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("setting hostname failed: {e:?}"))?;
    settings
        .set("mqtt-endpoint", &configuration.mqtt_endpoint.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("setting mqtt-endpoint failed: {e:?}"))?;
    settings
        .set(
            "availability-topic",
            &configuration.availability_topic.as_bytes(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("setting availability-topic failed: {e:?}"))?;
    settings
        .set("ota-topic", &configuration.ota_topic.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("setting ota-topic failed: {e:?}"))?;
    settings
        .set(
            "settings-topic-prefix",
            &configuration.settings_topic_prefix.as_bytes(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("setting settings-topic-prefix failed: {e:?}"))?;

    settings
        .set("siren-pin", &configuration.siren_pin)
        .await
        .map_err(|e| anyhow::anyhow!("setting siren-pin failed: {e:?}"))?;
    settings
        .set_serialized(
            "alarm-entity",
            &configuration.alarm_entity,
            &mut [0u8; 1024],
        )
        .await
        .map_err(|e| anyhow::anyhow!("setting alarm-entity failed: {e:?}"))?;
    if let Some(alarm_settings) = configuration.alarm_settings {
        settings
            .set_serialized("alarm-settings", &alarm_settings, &mut [0u8; 1024])
            .await
            .map_err(|e| anyhow::anyhow!("setting alarm-settings failed: {e:?}"))?;
    }
    settings
        .set_serialized(
            "motion-entities",
            &configuration.motion_entities,
            &mut [0u8; 4096],
        )
        .await
        .map_err(|e| anyhow::anyhow!("setting motion-entities failed: {e:?}"))?;

    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct Configuration {
    mac_address: String,
    hostname: String,
    mqtt_endpoint: String,
    availability_topic: String,
    ota_topic: String,
    settings_topic_prefix: String,
    siren_pin: u8,
    alarm_entity: HAEntity,
    alarm_settings: Option<AlarmSettings>,
    motion_entities: Vec<HAEntity>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AlarmSettings {
    #[serde(default = "default_alarm_state")]
    initial_state: AlarmState,
    #[serde(default = "default_arming_timeout")]
    arming_timeout: u16,
    #[serde(default = "default_pending_timeout")]
    pending_timeout: u16,
}
fn default_alarm_state() -> AlarmState {
    AlarmState::Disarmed
}
fn default_arming_timeout() -> u16 {
    90
}
fn default_pending_timeout() -> u16 {
    30
}

#[derive(Debug, Deserialize, Serialize)]
enum AlarmState {
    Disarmed,
    Armed,
    Triggered,
}

#[derive(Debug)]
struct MACAddress(Vec<u8>);
impl FromStr for MACAddress {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let octets: Result<Vec<_>, _> = s
            .split(':')
            .map(|raw_octet| u8::from_str_radix(raw_octet, 16))
            .collect();
        let octets = octets?;
        if octets.len() != 6 {
            anyhow::bail!(
                "MAC address should have 6 octets, but found {}",
                octets.len()
            );
        }
        Ok(Self(octets))
    }
}
impl MACAddress {
    fn addr(&self) -> &[u8] {
        &self.0
    }
}

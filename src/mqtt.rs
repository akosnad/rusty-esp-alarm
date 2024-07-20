use crate::StatusEvent;
use esp_idf_svc::{
    hal::task::thread::ThreadSpawnConfiguration,
    mqtt::client::{EspMqttClient, EventPayload, MqttClientConfiguration},
};
use std::sync::mpsc::Sender;

const MQTT_ENDPOINT: &str = env!("ESP_MQTT_ENDPOINT");

pub fn init(status_tx: Sender<StatusEvent>) -> anyhow::Result<()> {
    ThreadSpawnConfiguration {
        name: Some("mqtt\0".as_bytes()),
        stack_size: 8192,
        ..Default::default()
    }
    .set()?;

    let config = MqttClientConfiguration {
        client_id: Some("alarm"),
        ..Default::default()
    };

    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || {
            mqtt_task(status_tx.clone(), &config).unwrap_or_else(|e| {
                log::info!("MQTT task failed with: {:?}", e);
            });
            log::info!("MQTT task ended");
        })?;

    Ok(())
}

fn mqtt_task(
    status_tx: Sender<StatusEvent>,
    config: &MqttClientConfiguration<'_>,
) -> anyhow::Result<()> {
    log::info!("Connecting to MQTT server...");
    let (client, mut connection) = EspMqttClient::new(MQTT_ENDPOINT, config)?;

    while let Ok(event) = connection.next() {
        let payload = event.payload();

        match payload {
            EventPayload::Connected(_) => {
                log::info!("Connected to MQTT server");
                status_tx.send(StatusEvent::MqttConnected)?;
            }
            EventPayload::Disconnected => {
                log::info!("Disconnected from MQTT server, stopping client");
                std::thread::sleep(std::time::Duration::from_secs(3));
                status_tx.send(StatusEvent::MqttDisconnected)?;
                return Ok(());
            }
            EventPayload::Received {
                id: _,
                topic,
                data,
                details: _,
            } => {
                handle_mqtt_message(topic, data).unwrap_or_else(|e| {
                    log::info!("Error handling mqtt message: {:?}", e);
                });
            }
            _ => {}
        }
    }

    Ok(())
}

fn handle_mqtt_message(topic: Option<&str>, data: &[u8]) -> anyhow::Result<()> {
    if let Some(topic) = topic {
        log::info!("Received MQTT message from topic: {} {:?}", topic, data);
    } else {
        log::info!("Received MQTT message: {:?}", data);
    }

    Ok(())
}

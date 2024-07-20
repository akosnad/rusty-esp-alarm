use crate::StatusEvent;
use esp_idf_svc::{
    hal::task::thread::ThreadSpawnConfiguration,
    mqtt::client::{
        EspMqttClient, Event, LwtConfiguration, Message as _, MessageImpl, MqttClientConfiguration,
        QoS,
    },
};
use std::sync::mpsc::Sender;

const MQTT_ENDPOINT: &str = env!("ESP_MQTT_ENDPOINT");
const TOPIC_PREFIX: &str = env!("ESP_TOPIC_PREFIX");

pub fn init(status_tx: Sender<StatusEvent>) -> anyhow::Result<()> {
    ThreadSpawnConfiguration {
        name: Some("mqtt\0".as_bytes()),
        stack_size: 8192,
        ..Default::default()
    }
    .set()?;

    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || {
            let availability_topic = TOPIC_PREFIX.to_owned() + "/availability";
            let lwt = LwtConfiguration {
                topic: &availability_topic,
                payload: "offline".as_bytes(),
                retain: true,
                qos: QoS::AtLeastOnce,
            };
            let config = MqttClientConfiguration {
                client_id: Some("alarm"),
                keep_alive_interval: Some(std::time::Duration::from_secs(15)),
                network_timeout: std::time::Duration::from_secs(10),
                reconnect_timeout: Some(std::time::Duration::from_secs(30)),
                lwt: Some(lwt),
                out_buffer_size: 1024,
                task_stack: 8192,
                ..Default::default()
            };

            mqtt_task(status_tx.clone(), &config, &availability_topic).unwrap_or_else(|e| {
                log::info!("MQTT task failed with: {:?}", e);
            });
            log::info!("MQTT task ended");
        })?;

    Ok(())
}

fn mqtt_task(
    status_tx: Sender<StatusEvent>,
    config: &MqttClientConfiguration<'_>,
    availability_topic: &str,
) -> anyhow::Result<()> {
    log::info!("Connecting to MQTT server...");
    let (mut client, mut connection) = EspMqttClient::new_with_conn(MQTT_ENDPOINT, config)?;

    while let Some(result) = connection.next() {
        match result {
            Ok(event) => {
                let event: esp_idf_svc::mqtt::client::Event<MessageImpl> = event;

                match event {
                    Event::BeforeConnect => {}
                    Event::Connected(_) => {
                        log::info!("Connected to MQTT server");
                        status_tx.send(StatusEvent::MqttConnected)?;

                        log::info!("Sending birth message...");
                        client.publish(
                            availability_topic,
                            QoS::AtLeastOnce,
                            true,
                            "online".as_bytes(),
                        )?;
                        log::info!("Sent birth message");
                    }
                    Event::Disconnected => {
                        log::info!("Disconnected from MQTT server, stopping client");
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        status_tx.send(StatusEvent::MqttDisconnected)?;
                        return Ok(());
                    }
                    Event::Received(msg) => {
                        let topic = msg.topic();
                        let data = msg.data();
                        handle_mqtt_message(topic, data).unwrap_or_else(|e| {
                            log::info!("Error handling mqtt message: {:?}", e);
                        });
                    }
                    Event::Published(_) => {}
                    payload => {
                        log::warn!("Unhandled MQTT event: {:?}", payload);
                    }
                }
            }
            Err(e) => {
                anyhow::bail!("MQTT error: {:?}", e);
            }
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

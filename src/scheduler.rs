use crate::StatusEvent;
use esp_idf_svc::mqtt::client::{ConnState, EspMqttClient, MessageImpl, QoS};
use esp_idf_sys::EspError;
use ha_types::*;
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

pub struct AlarmEvent {
    // TODO
    pub time: u64,
    pub message: String,
}

pub fn scheduler_task(
    status_rx: Receiver<StatusEvent>,
    status_tx: Sender<StatusEvent>,
    alarm_event_queue: Arc<Mutex<VecDeque<AlarmEvent>>>,
) -> ! {
    let entities: Vec<HAEntity> = include!(concat!(env!("OUT_DIR"), "/entities.rs"));

    let mut mqtt_client = None;
    loop {
        let loop_result = || -> anyhow::Result<()> {
            loop {
                match status_rx.recv() {
                    Ok(event) => match event {
                        StatusEvent::EthConnected => {
                            log::info!("EthConnected");
                        }
                        StatusEvent::EthDisconnected => {
                            log::info!("EthDisconnected");
                        }
                        StatusEvent::MqttConnected(mut client) => {
                            init_mqtt(&mut client, &entities)?;
                            mqtt_client = Some(client);
                            log::info!("MqttConnected");
                        }
                        StatusEvent::MqttReconnected => {
                            if let Some(mut client) = mqtt_client.take() {
                                init_mqtt(&mut client, &entities)?;
                                mqtt_client = Some(client);
                            } else {
                                anyhow::bail!("MqttReconnected: mqtt client is None");
                            }
                            log::info!("MqttReconnected");
                        }
                        StatusEvent::MqttDisconnected => {
                            log::info!("MqttDisconnected");
                        }
                    },
                    Err(e) => {
                        log::error!("Error receiving status event: {:?}", e);
                    }
                }
            }
        }();
        if let Err(e) = loop_result {
            log::error!("Error in scheduler task: {:?}", e);
            log::info!("Restarting scheduler...");
        }
    }
}

fn init_mqtt(
    client: &mut EspMqttClient<'_, ConnState<MessageImpl, EspError>>,
    entities: &[HAEntity],
) -> anyhow::Result<()> {
    const AVAILABILITY_TOPIC: &str = env!("ESP_AVAILABILITY_TOPIC");
    // send entity config messages
    for entity in entities.iter() {
        let entity = HAEntity {
            availability: Some(HADeviceAvailability {
                payload_available: Some("online".to_string()),
                payload_not_available: Some("offline".to_string()),
                topic: AVAILABILITY_TOPIC.to_string(),
                value_template: None,
            }),
            ..entity.clone()
        };
        let topic = format!(
            "{}/{}/{}/config",
            "homeassistant", entity.variant, entity.unique_id
        );
        let payload = serde_json::to_string(&entity).unwrap();
        client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())?;
    }

    // birth message
    client.publish(AVAILABILITY_TOPIC, QoS::AtLeastOnce, true, b"online")?;

    Ok(())
}

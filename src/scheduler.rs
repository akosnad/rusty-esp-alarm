use crate::AlarmEvent;
use crate::StatusEvent;
use esp_idf_svc::mqtt::client::{ConnState, EspMqttClient, MessageImpl, QoS};
use esp_idf_sys::EspError;
use ha_types::*;
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

pub fn scheduler_task(
    entities: &[HAEntity],
    status_rx: Receiver<StatusEvent>,
    _status_tx: Sender<StatusEvent>,
    alarm_event_queue: Arc<Mutex<VecDeque<AlarmEvent>>>,
) -> ! {
    let mut mqtt_client = None;
    loop {
        let loop_result = || -> anyhow::Result<()> {
            loop {
                match status_rx.try_recv() {
                    Ok(event) => match event {
                        StatusEvent::EthConnected => {
                            log::info!("EthConnected");
                        }
                        StatusEvent::EthDisconnected => {
                            log::info!("EthDisconnected");
                        }
                        StatusEvent::MqttConnected(mut client) => {
                            init_mqtt(&mut client, entities)?;
                            mqtt_client = Some(client);
                            log::info!("MqttConnected");
                        }
                        StatusEvent::MqttReconnected => {
                            if let Some(mut client) = mqtt_client.take() {
                                init_mqtt(&mut client, entities)?;
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
                        if e == std::sync::mpsc::TryRecvError::Disconnected {
                            anyhow::bail!("status_rx disconnected");
                        }
                    }
                }

                // Skip processing events from the queue if the mqtt client is not available
                if let Some(mut client) = mqtt_client.take() {
                    match alarm_event_queue.try_lock() {
                        Ok(mut queue) => match queue.pop_front() {
                            Some(event) => match event {
                                AlarmEvent::MotionDetected(entity) => {
                                    send_binary_sensor_state(true, &entity, &mut client)?;
                                }
                                AlarmEvent::MotionCleared(entity) => {
                                    send_binary_sensor_state(false, &entity, &mut client)?;
                                }
                            },
                            None => {
                                // No new event to process
                            }
                        },
                        Err(e) => match e {
                            std::sync::TryLockError::WouldBlock => {
                                // Don't block this thread
                            }
                            std::sync::TryLockError::Poisoned(e) => {
                                anyhow::bail!("alarm_event_queue lock poisoned: {}", e);
                            }
                        },
                    }

                    // Done processing events, put the client back
                    mqtt_client = Some(client);
                }

                std::thread::sleep(std::time::Duration::from_millis(250));
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
    const OTA_TOPIC: &str = env!("ESP_OTA_TOPIC");

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
        let entity_out: HAEntityOut = entity.into();
        let payload = serde_json::to_string(&entity_out).unwrap();
        client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())?;
    }

    // birth message
    client.publish(AVAILABILITY_TOPIC, QoS::AtLeastOnce, true, b"online")?;

    // subscribe to ota
    client.subscribe(OTA_TOPIC, QoS::ExactlyOnce)?;

    Ok(())
}

fn send_binary_sensor_state(
    state: bool,
    entity: &HAEntity,
    client: &mut EspMqttClient<'_, ConnState<MessageImpl, EspError>>,
) -> anyhow::Result<()> {
    let payload = if state { "ON" } else { "OFF" };
    client.publish(
        &entity.state_topic,
        QoS::AtLeastOnce,
        true,
        payload.as_bytes(),
    )?;
    Ok(())
}

use crate::AlarmCommand;
use crate::AlarmEvent;
use crate::AlarmState;
use crate::StatusEvent;
use esp_idf_svc::mqtt::client::{EspMqttClient, QoS};
use esp_idf_svc::sys::esp_restart;
use ha_types::*;
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy)]
pub struct MqttSettings<'s> {
    pub availability_topic: &'s str,
    pub ota_topic: &'s str,
    pub settings_topic_prefix: &'s str,
}

pub fn scheduler_task(
    motion_entities: &[HAEntity],
    alarm_entity: HAEntity,
    status_rx: Receiver<StatusEvent>,
    _status_tx: Sender<StatusEvent>,
    alarm_event_queue: Arc<Mutex<VecDeque<AlarmEvent>>>,
    alarm_command_tx: Sender<AlarmCommand>,
    mqtt_settings: MqttSettings,
    settings: Arc<Mutex<crate::settings::Settings>>,
) -> ! {
    let mut mqtt_client = None;
    loop {
        let alarm_entity = alarm_entity.clone();
        let alarm_entity_command_topic = alarm_entity
            .command_topic
            .clone()
            .expect("Alarm entity has no command topic");
        let settings_set_topic_prefix = format!("{}/set", mqtt_settings.settings_topic_prefix);

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
                            init_mqtt(&mut client, motion_entities, &alarm_entity, mqtt_settings)?;
                            mqtt_client = Some(client);
                            log::info!("MqttConnected");
                        }
                        StatusEvent::MqttReconnected => {
                            if let Some(mut client) = mqtt_client.take() {
                                init_mqtt(
                                    &mut client,
                                    motion_entities,
                                    &alarm_entity,
                                    mqtt_settings,
                                )?;
                                mqtt_client = Some(client);
                            } else {
                                anyhow::bail!("MqttReconnected: mqtt client is None");
                            }
                            log::info!("MqttReconnected");
                        }
                        StatusEvent::MqttDisconnected => {
                            log::info!("MqttDisconnected");
                        }
                        StatusEvent::MqttMessage(msg) => {
                            if msg.topic == alarm_entity_command_topic {
                                handle_alarm_command(&msg.payload, &alarm_command_tx)?;
                            } else if msg.topic == settings_set_topic_prefix {
                                if let Some((key, val)) = msg.payload.split_once('\0') {
                                    if key.len() > 32 {
                                        anyhow::bail!(
                                            "invalid settings set command: key too large: {} (32 at max)",
                                            key.len()
                                        );
                                    }
                                    log::info!("MQTT set setting {key} to {val}");
                                    let mut settings = settings.lock().unwrap();
                                    handle_set_setting(key, val.as_bytes(), &mut settings)?;
                                }
                            }
                        }
                        StatusEvent::MqttMessageRaw { topic, payload } => {
                            if topic == settings_set_topic_prefix {
                                if let Some((key, val)) = payload.split_once(|v| *v == 0) {
                                    if key.len() > 32 {
                                        anyhow::bail!(
                                            "invalid settings set command: key too large: {} (32 at max)",
                                            key.len()
                                        );
                                    }
                                    let Ok(key) = str::from_utf8(key) else {
                                        anyhow::bail!(
                                            "invalid settings set command: key is not an UTF-8 string"
                                        );
                                    };
                                    if let Ok(val) = str::from_utf8(val) {
                                        log::info!("MQTT set setting {key} to {val}");
                                    } else {
                                        let len = val.len();
                                        log::info!(
                                            "MQTT set setting {key} to <binary of {len} byte(s)>"
                                        );
                                    }
                                    let mut settings = settings.lock().unwrap();
                                    handle_set_setting(key, val, &mut settings)?;
                                } else {
                                    anyhow::bail!(
                                        "invalid settings set command: could not find a null byte that splits the key and value"
                                    );
                                }
                            } else if let Some((_, key)) =
                                topic.split_once(settings_set_topic_prefix.as_str())
                            {
                                let mut settings = settings.lock().unwrap();
                                handle_set_setting(key, &payload, &mut settings)?;
                            }
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
                                AlarmEvent::AlarmStateChanged((entity, state)) => {
                                    send_alarm_state_change(&state, &entity, &mut client)?;
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
            log::error!("Error in scheduler task: {e:?}");
            log::info!("Restarting scheduler...");
        }
    }
}

fn init_mqtt(
    client: &mut EspMqttClient<'_>,
    entities: &[HAEntity],
    alarm_entity: &HAEntity,
    mqtt_setings: MqttSettings,
) -> anyhow::Result<()> {
    let MqttSettings {
        availability_topic,
        ota_topic,
        settings_topic_prefix,
    } = mqtt_setings;

    // send entity config messages
    for entity in entities.iter().chain([alarm_entity]) {
        let entity = HAEntity {
            availability: Some(HADeviceAvailability {
                payload_available: Some("online".to_string()),
                payload_not_available: Some("offline".to_string()),
                topic: availability_topic.to_string(),
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
        log::debug!("published config for entity: {}", entity_out.name);

        if let Some(command_topic) = entity_out.command_topic {
            client.subscribe(&command_topic, QoS::ExactlyOnce)?;
            log::debug!("subscribed to command topic: {command_topic}");
        }
    }

    // birth message
    client.publish(availability_topic, QoS::AtLeastOnce, true, b"online")?;
    log::debug!("sent birth message");

    // subscribe to ota
    client.subscribe(ota_topic, QoS::ExactlyOnce)?;
    log::debug!("subscribed to ota topic: {ota_topic}");

    // subscribe to settings topics
    client.subscribe(&format!("{settings_topic_prefix}/set"), QoS::ExactlyOnce)?;
    log::debug!("subscribed to settings topics with prefix: {settings_topic_prefix}");

    Ok(())
}

fn send_binary_sensor_state(
    state: bool,
    entity: &HAEntity,
    client: &mut EspMqttClient<'_>,
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

fn send_alarm_state_change(
    state: &AlarmState,
    entity: &HAEntity,
    client: &mut EspMqttClient<'_>,
) -> anyhow::Result<()> {
    let payload = match state {
        AlarmState::Disarmed => "disarmed",
        AlarmState::Arming(_) => "arming",
        AlarmState::Armed(_) => "armed_away",
        AlarmState::Pending(_) => "pending",
        AlarmState::Triggered => "triggered",
    };
    client.publish(
        &entity.state_topic,
        QoS::AtLeastOnce,
        true,
        payload.as_bytes(),
    )?;
    Ok(())
}

fn handle_alarm_command(
    payload: &str,
    alarm_command_tx: &Sender<AlarmCommand>,
) -> anyhow::Result<()> {
    let command = match payload.to_uppercase().as_str() {
        "ARM_AWAY" => AlarmCommand::Arm,
        "ARM_CUSTOM_BYPASS" => AlarmCommand::ArmInstantly,
        "DISARM" => AlarmCommand::Disarm,
        "PENDING" => AlarmCommand::ManualPending,
        "TRIGGER" => AlarmCommand::ManualTrigger,
        "UNTRIGGER" => AlarmCommand::Untrigger,
        "REBOOT" => unsafe {
            esp_restart();
        },
        _ => {
            log::warn!("Unknown command: {payload}");
            return Ok(());
        }
    };
    alarm_command_tx.send(command)?;
    Ok(())
}

fn handle_set_setting(
    key: &str,
    val: &[u8],
    settings: &mut crate::settings::Settings,
) -> anyhow::Result<()> {
    settings
        .set_blocking(key, &val)
        .map_err(|e| anyhow::anyhow!("failed to set setting {key}: {e:?}"))
}

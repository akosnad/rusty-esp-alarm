use std::time::Duration;
use std::{sync::mpsc, thread::JoinHandle};

use anyhow::bail;
use esp_idf_svc::hal::{cpu::Core, task::block_on};
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::mqtt::client::EventPayload;
use esp_idf_svc::sys::EspError;
use esp_idf_svc::{
    eth::{AsyncEth, EspEth},
    eventloop::EspSystemEventLoop,
    mqtt::client::{
        Details, EspMqttClient, InitialChunkData, LwtConfiguration, MqttClientConfiguration, QoS,
        SubsequentChunkData,
    },
    sys::{ESP_OK, esp_netif_set_hostname},
    timer::EspTaskTimerService,
};
use esp_ota::OtaUpdate;
use log::info;

use crate::{StatusEvent, spawn_task};

#[derive(Debug, Clone)]
pub struct NetworkSettings {
    pub hostname: String,
    pub mqtt_endpoint: String,
    pub availability_topic: String,
    pub ota_topic: String,
}

pub fn init<T>(
    eth: &'static mut EspEth<'_, T>,
    sys_loop: EspSystemEventLoop,
    timer: EspTaskTimerService,
    status_tx: mpsc::Sender<StatusEvent>,
    tasks: &mut Vec<JoinHandle<()>>,
    settings: &NetworkSettings,
) -> anyhow::Result<()> {
    let eth = AsyncEth::wrap(eth, sys_loop, timer)?;
    let status_tx_eth = status_tx.clone();
    let settings_clone = settings.clone();
    tasks.push(spawn_task(
        move || {
            block_on(eth_task(eth, status_tx_eth, &settings_clone));
        },
        "eth\0",
        Some(Core::Core0),
    )?);

    Ok(())
}

fn create_mqtt_client_config<'s>(availability_topic: &'s str) -> MqttClientConfiguration<'s> {
    MqttClientConfiguration {
        client_id: Some("alarm"),
        keep_alive_interval: Some(Duration::from_secs(15)),
        lwt: Some(LwtConfiguration {
            topic: availability_topic,
            payload: b"offline",
            qos: QoS::AtLeastOnce,
            retain: true,
        }),
        ..Default::default()
    }
}

async fn eth_task<T>(
    mut eth: AsyncEth<&mut EspEth<'_, T>>,
    status_tx: mpsc::Sender<StatusEvent>,
    settings: &NetworkSettings,
) -> ! {
    loop {
        eth.stop().await.unwrap_or_else(|e| {
            info!("failed to stop ethernet: {e}");
        });
        info!("Starting Ethernet...");
        async {
            let hostname = format!("{}{}", settings.hostname, '\0');
            unsafe {
                let result = esp_netif_set_hostname(
                    eth.eth().netif().handle(),
                    core::ffi::CStr::from_bytes_with_nul(hostname.clone().as_bytes())
                        .unwrap()
                        .as_ptr(),
                );
                if result != ESP_OK {
                    bail!("Failed to set hostname");
                }
            }
            eth.start().await?;

            info!("Connecting network...");
            while eth.wait_netif_up().await.is_err() {
                info!("Failed to connect to network, retrying in 5 seconds...");
                std::thread::sleep(Duration::from_secs(5));
            }

            status_tx
                .send(StatusEvent::EthConnected)
                .unwrap_or_else(|e| info!("failed to send status: {e}"));

            info!("Connected to network");

            loop {
                let status_tx = status_tx.clone();
                let settings = settings.clone();
                let mqtt_task_handle = spawn_task(
                    move || {
                        let status_tx_task = status_tx.clone();
                        let result = mqtt_task(
                            status_tx_task,
                            create_mqtt_client_config(settings.availability_topic.clone().as_str()),
                            &settings,
                        );
                        if let Err(e) = result {
                            log::error!("MQTT task failed: {e:?}");
                            status_tx
                                .send(StatusEvent::MqttDisconnected)
                                .unwrap_or_else(|e| {
                                    info!("failed to send status: {e}");
                                });
                        }
                    },
                    "mqtt\0",
                    Some(Core::Core0),
                )?;

                mqtt_task_handle.join().unwrap();

                if !eth.is_connected()? {
                    break;
                }
            }

            anyhow::bail!("Ethernet disconnected");
        }
        .await
        .unwrap_or_else(|_e: anyhow::Error| {
            info!("Restarting network in 5 seconds...");
            std::thread::sleep(Duration::from_secs(5));
            status_tx
                .send(StatusEvent::EthDisconnected)
                .unwrap_or_else(|e| {
                    info!("failed to send status: {e}");
                });
        });
    }
}

fn mqtt_task(
    status_tx: mpsc::Sender<StatusEvent>,
    mqtt_client_config: MqttClientConfiguration<'_>,
    settings: &NetworkSettings,
) -> anyhow::Result<()> {
    info!("Starting MQTT...");
    let (client, mut connection) =
        EspMqttClient::new(settings.mqtt_endpoint.as_str(), &mqtt_client_config)?;
    let mut client = Some(client);
    let mut ota = None;

    loop {
        match connection.next() {
            Err(e) => {
                info!("MQTT Message ERROR: {e}");
                break;
            }
            Ok(msg) => {
                let event = msg.payload();

                if let EventPayload::Connected(_) = event {
                    if let Some(client) = client.take() {
                        status_tx
                            .send(StatusEvent::MqttConnected(client))
                            .unwrap_or_else(|e| {
                                info!("failed to send status: {e}");
                            });
                    } else {
                        status_tx
                            .send(StatusEvent::MqttReconnected)
                            .unwrap_or_else(|e| {
                                info!("failed to send status: {e}");
                            });
                    }
                };

                if let EventPayload::Disconnected = event {
                    status_tx
                        .send(StatusEvent::MqttDisconnected)
                        .unwrap_or_else(|e| {
                            info!("failed to send status: {e}");
                        });
                };

                handle_mqtt_message(event, status_tx.clone(), &mut ota, settings).unwrap_or_else(
                    |e| {
                        info!("MQTT Message handling error: {e}");
                    },
                )
            }
        }
    }

    anyhow::bail!("MQTT disconnected");
}

fn handle_mqtt_message(
    event: EventPayload<'_, EspError>,
    status_tx: mpsc::Sender<StatusEvent>,
    ota: &mut Option<OtaUpdate>,
    settings: &NetworkSettings,
) -> anyhow::Result<()> {
    if let EventPayload::Received {
        id: _,
        topic,
        data,
        details,
    } = event
    {
        // Handle OTA messages
        //
        // Messages are sent in chunks, with only the first message containing the topic.
        // Subsequent messages (we assume they are subsequent, this depends on how esp_idf_svc
        // handles them) contain no topic. We can only guess if it's an OTA message by checking if
        // the OTA is in progress.
        //
        // TODO: the above is probably not true anymore; should revisit this implementation
        if topic == Some(settings.ota_topic.as_str()) || ota.is_some() {
            return handle_ota_message(data, details, ota);
        }

        match String::from_utf8(data.into()) {
            Ok(content) => {
                if let Some(topic) = topic {
                    info!("MQTT Message on topic {topic}: {content}");
                    status_tx
                        .send(StatusEvent::MqttMessage(crate::MqttMessage {
                            topic: String::from(topic),
                            payload: content,
                        }))
                        .expect("Failed to send status event");
                } else {
                    info!("MQTT Message: {content}");
                }
            }
            Err(_) => {
                if let Some(topic) = topic {
                    info!("MQTT binary message on topic {topic}");
                    status_tx
                        .send(StatusEvent::MqttMessageRaw {
                            topic: String::from(topic),
                            payload: Vec::from(data),
                        })
                        .expect("failed to send status event");
                } else {
                    info!("MQTT binary message received without topic");
                }
            }
        }
        Ok(())
    } else {
        Ok(())
    }
}

fn handle_ota_message(
    data: &[u8],
    details: Details,
    ota: &mut Option<OtaUpdate>,
) -> anyhow::Result<()> {
    if let Some(mut in_progress_ota) = ota.take() {
        match details {
            Details::InitialChunk(_) => {
                anyhow::bail!("Received initial OTA chunk while OTA is in progress");
            }
            Details::SubsequentChunk(SubsequentChunkData {
                current_data_offset,
                total_data_size,
            }) => {
                let current = current_data_offset + data.len();
                log::info!("OTA data: {current}/{total_data_size}");
                in_progress_ota
                    .write(data)
                    .expect("Failed to write OTA data");

                if current == total_data_size {
                    log::info!("OTA complete, applying...");
                    let mut completed_ota =
                        in_progress_ota.finalize().expect("Failed to finalize OTA");
                    if completed_ota.set_as_boot_partition().is_err() {
                        anyhow::bail!("Failed to set OTA as boot partition");
                    } else {
                        completed_ota.restart();
                    }
                } else {
                    ota.replace(in_progress_ota);
                    Ok(())
                }
            }
            Details::Complete => {
                log::info!("OTA complete, applying...");
                let mut completed_ota = in_progress_ota.finalize().expect("Failed to finalize OTA");
                if completed_ota.set_as_boot_partition().is_err() {
                    anyhow::bail!("Failed to set OTA as boot partition");
                } else {
                    completed_ota.restart();
                }
            }
        }
    } else {
        log::info!("Starting OTA...");
        match details {
            Details::InitialChunk(InitialChunkData { total_data_size }) => {
                log::info!("OTA data: 0/{total_data_size}");
                let mut new_ota = OtaUpdate::begin().expect("Failed to start OTA");
                new_ota.write(data).expect("Failed to write OTA data");
                ota.replace(new_ota);
                Ok(())
            }
            _ => {
                anyhow::bail!("Received OTA chunk without initial chunk");
            }
        }
    }
}

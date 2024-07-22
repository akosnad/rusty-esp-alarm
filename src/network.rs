use std::time::Duration;
use std::{sync::mpsc, thread::JoinHandle};

use anyhow::bail;
use esp_idf_hal::{cpu::Core, task::block_on};
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::{
    eth::{AsyncEth, EspEth},
    eventloop::EspSystemEventLoop,
    mqtt::client::{
        Details, EspMqttClient, InitialChunkData, LwtConfiguration, Message as _, MessageImpl,
        MqttClientConfiguration, QoS, SubsequentChunkData,
    },
    sys::{esp_netif_set_hostname, ESP_OK},
    timer::EspTaskTimerService,
};
use esp_ota::OtaUpdate;
use log::info;

use crate::{spawn_task, StatusEvent};

const MQTT_ENDPOINT: &str = env!("ESP_MQTT_ENDPOINT");
const AVAILABILITY_TOPIC: &str = env!("ESP_AVAILABILITY_TOPIC");
const OTA_TOPIC: &str = env!("ESP_OTA_TOPIC");

pub fn init<T>(
    eth: &'static mut EspEth<'_, T>,
    sys_loop: EspSystemEventLoop,
    timer: EspTaskTimerService,
    status_tx: mpsc::Sender<StatusEvent>,
    tasks: &mut Vec<JoinHandle<()>>,
) -> anyhow::Result<()> {
    let eth = AsyncEth::wrap(eth, sys_loop, timer)?;
    let status_tx_eth = status_tx.clone();
    tasks.push(spawn_task(
        move || {
            block_on(eth_task(eth, status_tx_eth));
        },
        "eth\0",
        Some(Core::Core0),
    )?);

    Ok(())
}

fn create_mqtt_client_config() -> MqttClientConfiguration<'static> {
    MqttClientConfiguration {
        client_id: Some("alarm"),
        keep_alive_interval: Some(Duration::from_secs(15)),
        lwt: Some(LwtConfiguration {
            topic: AVAILABILITY_TOPIC,
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
) -> ! {
    loop {
        eth.stop().await.unwrap_or_else(|e| {
            info!("failed to stop ethernet: {}", e);
        });
        info!("Starting Ethernet...");
        async {
            const HOSTNAME: &str = "alarm\0";
            unsafe {
                let result = esp_netif_set_hostname(
                    eth.eth().netif().handle(),
                    core::ffi::CStr::from_bytes_with_nul(HOSTNAME.as_bytes())
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
                .unwrap_or_else(|e| info!("failed to send status: {}", e));

            info!("Connected to network");

            loop {
                let status_tx = status_tx.clone();
                let mqtt_task_handle = spawn_task(
                    move || {
                        let status_tx_task = status_tx.clone();
                        let result = mqtt_task(status_tx_task, create_mqtt_client_config());
                        if result.is_err() {
                            status_tx
                                .send(StatusEvent::MqttDisconnected)
                                .unwrap_or_else(|e| {
                                    info!("failed to send status: {}", e);
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
                    info!("failed to send status: {}", e);
                });
        });
    }
}

fn mqtt_task(
    status_tx: mpsc::Sender<StatusEvent>,
    mqtt_client_config: MqttClientConfiguration<'_>,
) -> anyhow::Result<()> {
    info!("Starting MQTT...");
    let (client, mut connection) =
        EspMqttClient::new_with_conn(MQTT_ENDPOINT, &mqtt_client_config)?;
    let mut client = Some(client);
    let mut ota = None;

    while let Some(msg) = connection.next() {
        match msg {
            Err(e) => info!("MQTT Message ERROR: {}", e),
            Ok(msg) => {
                let event: esp_idf_svc::mqtt::client::Event<MessageImpl> = msg;

                if let esp_idf_svc::mqtt::client::Event::Connected(_) = event {
                    if let Some(client) = client.take() {
                        status_tx
                            .send(StatusEvent::MqttConnected(client))
                            .unwrap_or_else(|e| {
                                info!("failed to send status: {}", e);
                            });
                    } else {
                        status_tx
                            .send(StatusEvent::MqttReconnected)
                            .unwrap_or_else(|e| {
                                info!("failed to send status: {}", e);
                            });
                    }
                };

                if let esp_idf_svc::mqtt::client::Event::Disconnected = event {
                    status_tx
                        .send(StatusEvent::MqttDisconnected)
                        .unwrap_or_else(|e| {
                            info!("failed to send status: {}", e);
                        });
                };

                handle_mqtt_message(event, status_tx.clone(), &mut ota).unwrap_or_else(|e| {
                    info!("MQTT Message handling error: {}", e);
                })
            }
        }
    }

    anyhow::bail!("MQTT disconnected");
}

fn handle_mqtt_message(
    event: esp_idf_svc::mqtt::client::Event<MessageImpl>,
    status_tx: mpsc::Sender<StatusEvent>,
    ota: &mut Option<OtaUpdate>,
) -> anyhow::Result<()> {
    if let esp_idf_svc::mqtt::client::Event::Received(msg) = event {
        let topic = msg.topic();

        // Handle OTA messages
        //
        // Messages are sent in chunks, with the first message ony containing the topic.
        // The rest of the messages contain no topic, we can only guess if it's an OTA message
        // by checking if the OTA is in progress.
        if topic == Some(OTA_TOPIC) || ota.is_some() {
            return handle_ota_message(msg, ota);
        }

        let content = String::from_utf8(msg.data().into())?;
        if let Some(topic) = topic {
            info!("MQTT Message on topic {}: {}", topic, content);
            status_tx
                .send(StatusEvent::MqttMessage(crate::MqttMessage {
                    topic: String::from(topic),
                    payload: content,
                }))
                .expect("Failed to send status event");
        } else {
            info!("MQTT Message: {}", content);
        }
        Ok(())
    } else {
        Ok(())
    }
}

fn handle_ota_message(msg: MessageImpl, ota: &mut Option<OtaUpdate>) -> anyhow::Result<()> {
    let data = msg.data();
    if let Some(mut in_progress_ota) = ota.take() {
        match msg.details() {
            Details::InitialChunk(_) => {
                anyhow::bail!("Received initial OTA chunk while OTA is in progress");
            }
            Details::SubsequentChunk(SubsequentChunkData {
                current_data_offset,
                total_data_size,
            }) => {
                let current = current_data_offset + data.len();
                log::info!("OTA data: {}/{}", current, total_data_size);
                in_progress_ota
                    .write(data)
                    .expect("Failed to write OTA data");

                if current == *total_data_size {
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
        match msg.details() {
            Details::InitialChunk(InitialChunkData { total_data_size }) => {
                log::info!("OTA data: 0/{}", total_data_size);
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

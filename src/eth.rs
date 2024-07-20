use anyhow::bail;
use esp_idf_svc::{
    eth::EspEth,
    eventloop::EspSystemEventLoop,
    hal::task::thread::ThreadSpawnConfiguration,
    handle::RawHandle,
    ipv4, ping,
    sys::{esp_netif_set_hostname, ESP_OK},
};
use log::info;

pub fn init<T>(sysloop: EspSystemEventLoop, eth: &'static mut EspEth<'_, T>) -> anyhow::Result<()> {
    ThreadSpawnConfiguration {
        name: Some("eth\0".as_bytes()),
        ..Default::default()
    }
    .set()?;

    std::thread::spawn(move || loop {
        let loop_result = || -> anyhow::Result<()> {
            while let Err(e) = eth_configure(&sysloop, eth) {
                info!("Failed to configure eth: {:?}", e);
                eth.stop()?;
                info!("Retrying in 3 seconds...");
                std::thread::sleep(std::time::Duration::from_secs(3));
            }

            info!("Ethernet set up successfully");

            while eth.is_connected()? {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            log::info!("Ethernet disconnected");
            eth.stop()?;
            Ok(())
        }();
        if let Err(e) = loop_result {
            log::error!("Error in eth loop: {:?}", e);
        }
        log::info!("Restarting ethernet...");
    });

    Ok(())
}

fn eth_configure<T>(
    sysloop: &EspSystemEventLoop,
    eth: &mut esp_idf_svc::eth::EspEth<'_, T>,
) -> anyhow::Result<()> {
    info!("Eth created");

    const HOSTNAME: &str = "alarm\0";
    unsafe {
        let result = esp_netif_set_hostname(
            eth.netif().handle(),
            core::ffi::CStr::from_bytes_with_nul(HOSTNAME.as_bytes())
                .unwrap()
                .as_ptr(),
        );
        if result != ESP_OK {
            bail!("Failed to set hostname");
        }
    }

    let mut eth = esp_idf_svc::eth::BlockingEth::wrap(eth, sysloop.clone())?;

    info!("Starting eth...");

    eth.start()?;

    info!("Waiting for DHCP lease...");

    eth.wait_netif_up()?;

    let ip_info = eth.eth().netif().get_ip_info()?;

    info!("Eth DHCP info: {:?}", ip_info);

    //ping(ip_info.subnet.gateway)?;

    Ok(())
}

#[allow(dead_code)]
fn ping(ip: ipv4::Ipv4Addr) -> anyhow::Result<()> {
    info!("About to do some pings for {:?}", ip);

    let ping_summary = ping::EspPing::default().ping(ip, &Default::default())?;
    if ping_summary.transmitted != ping_summary.received {
        bail!("Pinging IP {} resulted in timeouts", ip);
    }

    info!("Pinging done");

    Ok(())
}

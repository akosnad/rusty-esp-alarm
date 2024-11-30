#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use core::str::FromStr;

use defmt::info;
use embassy_executor::task;
use embassy_futures::yield_now;
use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_hal::{
    dma::{Dma, DmaPriority, DmaRxBuf, DmaTxBuf},
    dma_buffers,
    gpio::{Input, Level, Output, Pull},
    prelude::*,
    spi::AnySpi,
};
use esp_println as _;
use rusty_esp_alarm as lib;

extern crate alloc;

macro_rules! make_static {
    ($e:expr) => {{
        let boxed = alloc::boxed::Box::new($e);
        alloc::boxed::Box::leak(boxed)
    }};
}

type EthSpi = lib::EthSpi<'static, AnySpi>;
type EthChip = embassy_net_wiznet::chip::W5500;
type EthDevice = embassy_net_wiznet::Device<'static>;
type EthDeviceRunner = embassy_net_wiznet::Runner<'static, EthChip, EthSpi, Input<'static>, Output<'static>>;
type NetStack = embassy_net::Stack<'static>;
type NetStackRunner = embassy_net::Runner<'static, EthDevice>;

#[esp_hal_embassy::main]
async fn main(spawner: embassy_executor::Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    esp_hal_embassy::init(timg0.timer0);

    heap_allocator!(32 * 1024);

    let mut trng = esp_hal::rng::Trng::new(peripherals.RNG, peripherals.ADC1);

    // Ethernet
    info!("Initializing Ethernet...");
    let sclk = peripherals.GPIO18;
    let miso = peripherals.GPIO23;
    let mosi = peripherals.GPIO19;
    let cs = peripherals.GPIO5;
    let int = Input::new(peripherals.GPIO26, Pull::None);
    let rst = Output::new(peripherals.GPIO33, Level::Low);

    let dma = Dma::new(peripherals.DMA);
    let dma_channel = dma.spi2channel;
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(1024);
    let dma_rx_buf =
        DmaRxBuf::new(rx_descriptors, rx_buffer).expect("Failed to create DMA RX buffer");
    let dma_tx_buf =
        DmaTxBuf::new(tx_descriptors, tx_buffer).expect("Failed to create DMA TX buffer");

    let spi = esp_hal::spi::master::Spi::new_with_config(
        peripherals.SPI2,
        esp_hal::spi::master::Config {
            frequency: 20u32.MHz(),
            mode: esp_hal::spi::SpiMode::Mode0,
            ..Default::default()
        },
    )
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_miso(miso)
    .with_cs(cs)
    .with_dma(dma_channel.configure(false, DmaPriority::Priority0))
    .with_buffers(dma_rx_buf, dma_tx_buf)
    .into_async();
    let spi = lib::EthSpi(spi);

    let state = make_static!(embassy_net_wiznet::State::<8, 8>::new());
    let (eth, net): (EthDevice, EthDeviceRunner) = embassy_net_wiznet::new::<8, 8, _, _, _, _>(
        [0x02, 0, 0, 0xFC, 0x18, 0x1E],
        state,
        spi,
        int,
        rst,
    )
    .await;
    spawner
        .spawn(net_task(net))
        .expect("Failed to spawn net_task");

    let stack_resources = make_static!(embassy_net::StackResources::<3>::new());
    let dhcp_config = {
        let mut config = embassy_net::DhcpConfig::default();
        // TODO: read from config
        config.hostname = Some(heapless::String::from_str("alarm-test").expect("Failed to create hostname"));
        config
    };
    let seed = {
        let mut buf = [0u8; u64::BITS as usize / 8];
        trng.read(&mut buf);
        u64::from_le_bytes(buf)
    };
    let (net_stack, net_runner): (NetStack, NetStackRunner) = embassy_net::new(
        eth,
        embassy_net::Config::dhcpv4(dhcp_config),
        stack_resources,
        seed,
    );
    spawner.spawn(net_runner_task(net_runner)).expect("Failed to spawn net_runner_task");

    info!("Ethernet initialized, waiting for link...");

    let config = wait_for_config(&net_stack).await;
    info!("Link up, IP: {}", config.address);
}

#[task]
async fn net_task(net: EthDeviceRunner) {
    net.run().await;
}

#[task]
async fn net_runner_task(mut runner: NetStackRunner) {
    runner.run().await;
}

async fn wait_for_config(stack: &NetStack) -> embassy_net::StaticConfigV4 {
    loop {
        if let Some(config) = stack.config_v4() {
            return config;
        }
        yield_now().await;
    }
}

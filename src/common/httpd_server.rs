use embedded_svc::http::Headers;
use esp_idf_hal::io::{Read, Write};
use esp_idf_svc::http::{server::EspHttpServer, Method};
use esp_idf_sys::esp_restart;
use log::info;
use serde::Deserialize;

use crate::wifi::ssid_manager::SsidMananger;

pub fn create_server() -> anyhow::Result<EspHttpServer<'static>> {
    const STACK_SIZE: usize = 10240;
    let server_configuration = esp_idf_svc::http::server::Configuration {
        stack_size: STACK_SIZE,
        max_resp_headers: 4096,
        ..Default::default()
    };

    Ok(EspHttpServer::new(&server_configuration)?)
}

static INDEX_HTML: &str = include_str!("../../assets/html/config_wifi.html");

#[derive(Deserialize)]
struct FormData<'a> {
    wifi_ssid: &'a str,
    wifi_password: &'a str,
}

pub fn start_http_server(http_server: &mut EspHttpServer<'static>) -> anyhow::Result<()> {
    // let mut http_server = create_server()?;

    http_server.fn_handler("/", Method::Get, |req| {
        req.into_ok_response()?
            .write_all(INDEX_HTML.as_bytes())
            .map(|_| ())
    })?;

    // http_server.fn_handler::<anyhow::Error, _>("/hello", Method::Get, |req| {
    //     // req.into_ok_response()?
    //     //     .write_all(INDEX_HTML.as_bytes())
    //     //     .map(|_| ());

    //     let mut resp = req.into_ok_response()?;
    //     write!(
    //         resp,
    //         "SSID: {}  Password: {}!",
    //         "wifi_ssid", "wifi_password"
    //     )?;
    //     Ok(())
    // })?;

    const MAX_LEN: usize = 2048;
    http_server.fn_handler::<anyhow::Error, _>("/config_wifi", Method::Post, |mut req| {
        let len = req.content_len().unwrap_or(0) as usize;

        if len > MAX_LEN {
            req.into_status_response(413)?
                .write_all("Request too big".as_bytes())?;
            return Ok(());
        }

        let mut buf = vec![0; len];
        req.read_exact(&mut buf)?;
        let mut resp = req.into_ok_response()?;

        if let Ok(form) = serde_json::from_slice::<FormData>(&buf) {
            let wifi_ssid = form.wifi_ssid;
            let wifi_password = form.wifi_password;

            info!(
                "WiFi config: SSID={}, Password={}",
                wifi_ssid, wifi_password
            );

            let mut ssid_manager = SsidMananger::get_instance();

            if let Err(e) = ssid_manager.add_ssid(wifi_ssid, wifi_password) {
                log::error!("添加新wifi失败: {:?}", e);
            }

            resp.write_all("OK!".as_bytes()).map(|_| ())?;

            // write!(resp, "SSID: {}  Password: {}!", wifi_ssid, wifi_password)?;

            // TODO:: 临时方案：在配置完wifi后，重启。以后应该是使用消息系统，使application进入wifi连接状态，如果连接不成功再进入这个配置页面。
            unsafe {
                esp_restart();
            }
        } else {
            resp.write_all("Invalid form data".as_bytes())?;
        }

        Ok(())
    })?;
    Ok(())
}

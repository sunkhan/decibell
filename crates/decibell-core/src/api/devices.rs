//! The account's devices and its password (docs/superpowers/specs/
//! 2026-10-19-devices-and-password-design.md): list the devices signed in,
//! sign some out, change the password. Each is a request to central whose
//! answer the router (net/central.rs) hands to the command waiting on it.

use std::time::Duration;

use tokio::sync::oneshot;

use crate::api::CoreError;
use crate::config;
use crate::events::DevicePayload;
use crate::net::connection::build_packet;
use crate::net::proto::{packet, DeviceListRequest, DeviceSignOutRequest, PasswordChangeRequest};
use crate::state::{self, AppState};

/// Send `payload` to central and wait up to `wait` for its answer, which
/// the router puts in the slot `slot` picks out of AppState. A second
/// request of the same kind while one waits replaces it (the first fails).
pub(crate) async fn ask_central<R>(
    kind: packet::Type,
    payload: packet::Payload,
    slot: fn(&mut AppState) -> &mut Option<oneshot::Sender<R>>,
    wait: Duration,
) -> Result<R, CoreError> {
    let state_arc = state::shared();
    let (write_tx, data, rx) = {
        let mut s = state_arc.lock().await;
        let token = s
            .token
            .clone()
            .ok_or_else(|| CoreError::other("Not signed in"))?;
        let tx = s
            .central
            .as_ref()
            .and_then(|c| c.connection_write_tx())
            .ok_or_else(|| CoreError::other("Not connected to the server"))?;
        let (wtx, wrx) = oneshot::channel();
        *slot(&mut s) = Some(wtx);
        (tx, build_packet(kind, payload, Some(&token)), wrx)
    };
    match tokio::time::timeout(Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => {}
        _ => {
            *slot(&mut *state_arc.lock().await) = None;
            return Err(CoreError::other("Couldn't reach the server"));
        }
    }
    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(res)) => Ok(res),
        Ok(Err(_)) => Err(CoreError::other("Connection closed before the reply")),
        Err(_) => {
            *slot(&mut *state_arc.lock().await) = None;
            Err(CoreError::other("The server didn't answer"))
        }
    }
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct DeviceEntry {
    /// Central's handle for it (`sign_out_devices`).
    pub id: i64,
    /// The operating system's name, as the device reported it.
    pub name: String,
    /// "linux" / "windows" / "macos".
    pub platform: String,
    pub app_version: String,
    /// The address it last signed in or reconnected from.
    pub address: String,
    /// Unix seconds: its last password sign-in.
    pub signed_in_at: i64,
    /// Unix seconds: last seen.
    pub last_active_at: i64,
    pub online: bool,
    /// This device.
    pub current: bool,
}

impl From<DevicePayload> for DeviceEntry {
    fn from(d: DevicePayload) -> Self {
        DeviceEntry {
            id: d.id,
            name: d.name,
            platform: d.platform,
            app_version: d.app_version,
            address: d.address,
            signed_in_at: d.signed_in_at,
            last_active_at: d.last_active_at,
            online: d.online,
            current: d.current,
        }
    }
}

/// The devices signed in to the account, most recently active first.
pub async fn list_devices() -> Result<Vec<DeviceEntry>, CoreError> {
    crate::rt::run(async move {
        let res = ask_central(
            packet::Type::DeviceListReq,
            packet::Payload::DeviceListReq(DeviceListRequest {}),
            |s| &mut s.pending_device_list,
            Duration::from_secs(10),
        )
        .await?;
        if !res.success {
            return Err(CoreError::refused("The server couldn't list your devices"));
        }
        Ok(res
            .devices
            .into_iter()
            .map(|d| DeviceEntry::from(DevicePayload::from(d)))
            .collect())
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct SignOutDevicesArgs {
    /// The devices to sign out (this device's own id is ignored).
    pub ids: Vec<i64>,
    /// Every device but this one, instead.
    pub all_others: Option<bool>,
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct SignOutDevicesResult {
    pub signed_out: u32,
}

/// Sign devices out: central closes their connections (they sign out and
/// forget their saved sign-in), their reconnect tokens stop working, and
/// communities drop their sessions within a minute.
pub async fn sign_out_devices(args: SignOutDevicesArgs) -> Result<SignOutDevicesResult, CoreError> {
    crate::rt::run(async move {
        let res = ask_central(
            packet::Type::DeviceSignOutReq,
            packet::Payload::DeviceSignOutReq(DeviceSignOutRequest {
                ids: args.ids,
                all_others: args.all_others.unwrap_or(false),
                this_device: false,
            }),
            |s| &mut s.pending_device_sign_out,
            Duration::from_secs(10),
        )
        .await?;
        if !res.success {
            return Err(CoreError::refused(res.message));
        }
        Ok(SignOutDevicesResult {
            signed_out: res.signed_out.max(0) as u32,
        })
    })
    .await
}

/// Logging out: central forgets this device, so its saved password can't
/// sign it back in and communities drop its sessions. Best effort and
/// bounded: an offline device logs out all the same.
pub(crate) async fn forget_this_device() {
    let asked = ask_central(
        packet::Type::DeviceSignOutReq,
        packet::Payload::DeviceSignOutReq(DeviceSignOutRequest {
            ids: Vec::new(),
            all_others: false,
            this_device: true,
        }),
        |s| &mut s.pending_device_sign_out,
        Duration::from_secs(3),
    )
    .await;
    if let Err(e) = asked {
        log::info!("[auth] central didn't confirm forgetting this device: {}", e.message());
    }
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct ChangePasswordArgs {
    pub current_password: String,
    pub new_password: String,
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct ChangePasswordResult {
    /// Other devices signed out by the change.
    pub signed_out: u32,
}

/// Change the account's password. Central checks the current one and signs
/// every other device out; this one stays signed in with a new reconnect
/// token, and its saved sign-in (if any) takes the new password.
pub async fn change_password(args: ChangePasswordArgs) -> Result<ChangePasswordResult, CoreError> {
    crate::rt::run(async move {
        let res = ask_central(
            packet::Type::PasswordChangeReq,
            packet::Payload::PasswordChangeReq(PasswordChangeRequest {
                current_password: args.current_password,
                new_password: args.new_password.clone(),
            }),
            |s| &mut s.pending_password_change,
            // Two password hashes on central's side.
            Duration::from_secs(20),
        )
        .await?;
        if !res.success {
            return Err(CoreError::refused(res.message));
        }
        {
            let state_arc = state::shared();
            let mut s = state_arc.lock().await;
            if !res.resume_token.is_empty() {
                s.resume_token = Some(res.resume_token.clone());
            }
            // A reconnect that falls back to the password uses the new one.
            if let Some((user, _)) = s.credentials.clone() {
                s.credentials = Some((user, args.new_password.clone()));
            }
        }
        if let Ok(cfg) = config::load() {
            if let Some(saved) = cfg.credentials {
                let creds = config::Credentials {
                    username: saved.username,
                    password: args.new_password,
                };
                if let Err(e) = config::save(Some(&creds), &cfg.settings) {
                    log::warn!("[auth] couldn't save the new password: {}", e);
                }
            }
        }
        Ok(ChangePasswordResult {
            signed_out: res.signed_out.max(0) as u32,
        })
    })
    .await
}

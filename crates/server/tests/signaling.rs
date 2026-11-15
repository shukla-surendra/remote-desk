//! End-to-end tests: real server on an ephemeral port, raw WebSocket clients.

use std::{net::SocketAddr, time::Duration};

use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHasher,
};
use futures_util::{SinkExt, StreamExt};
use proto::*;
use remote_desk_server::Config;
use serde::{de::DeserializeOwned, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start(config: Config) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(remote_desk_server::serve(listener, config));
    addr
}

async fn connect(addr: SocketAddr, path: &str) -> Ws {
    tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
        .await
        .unwrap()
        .0
}

async fn send<T: Serialize>(ws: &mut Ws, m: &T) {
    ws.send(Message::Text(serde_json::to_string(m).unwrap().into()))
        .await
        .unwrap();
}

async fn recv<T: DeserializeOwned>(ws: &mut Ws) -> T {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for message")
            .expect("stream ended")
            .unwrap();
        if let Message::Text(t) = frame {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// Asserts the server closes the socket (after any pending frames are read).
async fn assert_closed(ws: &mut Ws) {
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();
    assert!(
        matches!(next, None | Some(Ok(Message::Close(_))) | Some(Err(_))),
        "{next:?}"
    );
}

fn hash(pw: &str) -> String {
    Argon2::default()
        .hash_password(pw.as_bytes(), &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string()
}

fn info() -> AgentInfo {
    AgentInfo {
        hostname: "test-host".into(),
        os: "test".into(),
        agent_version: "0".into(),
    }
}

async fn register(
    addr: SocketAddr,
    pw: &str,
    prev: Option<(String, String)>,
) -> (Ws, String, String) {
    let mut ws = connect(addr, "/ws/agent").await;
    let (device_id, device_token) = prev.map(|(a, b)| (Some(a), Some(b))).unwrap_or_default();
    send(
        &mut ws,
        &AgentToServer::Register {
            protocol_version: PROTOCOL_VERSION,
            device_id,
            device_token,
            password_hash: hash(pw),
            info: info(),
        },
    )
    .await;
    match recv(&mut ws).await {
        ServerToAgent::Registered {
            device_id,
            device_token,
            ..
        } => (ws, device_id, device_token),
        other => panic!("unexpected {other:?}"),
    }
}

async fn viewer(addr: SocketAddr, device_id: &str, pw: &str) -> (Ws, ServerToViewer) {
    let mut ws = connect(addr, "/ws/viewer").await;
    send(
        &mut ws,
        &ViewerToServer::Connect {
            protocol_version: PROTOCOL_VERSION,
            device_id: device_id.into(),
            password: pw.into(),
        },
    )
    .await;
    let first = recv(&mut ws).await;
    (ws, first)
}

#[tokio::test]
async fn full_session_flow() {
    let addr = start(Config::default()).await;
    let (mut agent, id, _) = register(addr, "hunter22", None).await;
    assert_eq!(id.len(), 9);

    // Spaces in the typed ID are tolerated ("123 456 789").
    let spaced = format!("{} {} {}", &id[..3], &id[3..6], &id[6..]);
    let (mut v, first) = viewer(addr, &spaced, "hunter22").await;
    let ServerToViewer::AwaitingConsent { session_id } = first else {
        panic!("{first:?}")
    };

    let ServerToAgent::SessionRequest {
        session_id: sid, ..
    } = recv(&mut agent).await
    else {
        panic!()
    };
    assert_eq!(sid, session_id);

    // Signals sent before consent must not reach the agent.
    send(
        &mut v,
        &ViewerToServer::Signal {
            payload: SignalPayload::Answer {
                sdp: "early".into(),
            },
        },
    )
    .await;

    send(
        &mut agent,
        &AgentToServer::SessionDecision {
            session_id,
            accept: true,
        },
    )
    .await;
    match recv(&mut v).await {
        ServerToViewer::SessionStarted {
            session_id: s,
            info: i,
            ..
        } => {
            assert_eq!(s, session_id);
            assert_eq!(i.hostname, "test-host");
        }
        other => panic!("{other:?}"),
    }

    let offer = SignalPayload::Offer {
        sdp: "v=0 offer".into(),
    };
    send(
        &mut agent,
        &AgentToServer::Signal {
            session_id,
            payload: offer.clone(),
        },
    )
    .await;
    assert_eq!(
        recv::<ServerToViewer>(&mut v).await,
        ServerToViewer::Signal { payload: offer }
    );

    let answer = SignalPayload::Answer {
        sdp: "v=0 answer".into(),
    };
    send(
        &mut v,
        &ViewerToServer::Signal {
            payload: answer.clone(),
        },
    )
    .await;
    assert_eq!(
        recv::<ServerToAgent>(&mut agent).await,
        ServerToAgent::Signal {
            session_id,
            payload: answer
        }
    );

    // A second viewer is refused while the session is active.
    let (_v2, busy) = viewer(addr, &id, "hunter22").await;
    assert!(matches!(
        busy,
        ServerToViewer::Error(ErrorBody {
            code: ErrorCode::DeviceBusy,
            ..
        })
    ));

    // Viewer leaving ends the session on the agent side and frees the device.
    v.close(None).await.unwrap();
    assert_eq!(
        recv::<ServerToAgent>(&mut agent).await,
        ServerToAgent::SessionEnded { session_id }
    );
    let (_v3, again) = viewer(addr, &id, "hunter22").await;
    assert!(matches!(again, ServerToViewer::AwaitingConsent { .. }));
}

#[tokio::test]
async fn rejected_consent_closes_viewer() {
    let addr = start(Config::default()).await;
    let (mut agent, id, _) = register(addr, "hunter22", None).await;
    let (mut v, _) = viewer(addr, &id, "hunter22").await;
    let ServerToAgent::SessionRequest { session_id, .. } = recv(&mut agent).await else {
        panic!()
    };
    send(
        &mut agent,
        &AgentToServer::SessionDecision {
            session_id,
            accept: false,
        },
    )
    .await;
    assert_eq!(
        recv::<ServerToViewer>(&mut v).await,
        ServerToViewer::SessionEnded {
            reason: EndReason::Rejected
        }
    );
    assert_closed(&mut v).await;
}

#[tokio::test]
async fn agent_disconnect_ends_session() {
    let addr = start(Config::default()).await;
    let (mut agent, id, _) = register(addr, "hunter22", None).await;
    let (mut v, _) = viewer(addr, &id, "hunter22").await;
    let _: ServerToAgent = recv(&mut agent).await;
    agent.close(None).await.unwrap();
    assert_eq!(
        recv::<ServerToViewer>(&mut v).await,
        ServerToViewer::SessionEnded {
            reason: EndReason::AgentDisconnected
        }
    );
}

#[tokio::test]
async fn wrong_password_and_unknown_device_look_identical() {
    let addr = start(Config::default()).await;
    let (_agent, id, _) = register(addr, "hunter22", None).await;
    let (_, a) = viewer(addr, &id, "nope").await;
    let (_, b) = viewer(addr, "999999999", "hunter22").await;
    assert_eq!(a, b);
    assert!(matches!(
        a,
        ServerToViewer::Error(ErrorBody {
            code: ErrorCode::InvalidCredentials,
            ..
        })
    ));
}

#[tokio::test]
async fn failed_logins_are_rate_limited() {
    let addr = start(Config {
        max_failed_logins: 2,
        ..Config::default()
    })
    .await;
    let (_agent, id, _) = register(addr, "hunter22", None).await;
    for _ in 0..2 {
        viewer(addr, &id, "bad").await;
    }
    // Even the correct password is refused once limited.
    let (_, r) = viewer(addr, &id, "hunter22").await;
    assert!(matches!(
        r,
        ServerToViewer::Error(ErrorBody {
            code: ErrorCode::RateLimited,
            ..
        })
    ));
}

#[tokio::test]
async fn device_id_is_reclaimable_only_with_token() {
    let addr = start(Config::default()).await;
    let (a1, id, token) = register(addr, "hunter22", None).await;

    // Wrong token: refused.
    let mut imposter = connect(addr, "/ws/agent").await;
    send(
        &mut imposter,
        &AgentToServer::Register {
            protocol_version: PROTOCOL_VERSION,
            device_id: Some(id.clone()),
            device_token: Some("x".repeat(43)),
            password_hash: hash("attacker1"),
            info: info(),
        },
    )
    .await;
    assert!(matches!(
        recv::<ServerToAgent>(&mut imposter).await,
        ServerToAgent::Error(ErrorBody {
            code: ErrorCode::DeviceIdTaken,
            ..
        })
    ));

    // Right token: takes over (even while the old connection is still open) and
    // the new password applies.
    let (_a2, id2, _) = register(addr, "newpass22", Some((id.clone(), token))).await;
    assert_eq!(id, id2);
    drop(a1);
    let (_, old) = viewer(addr, &id, "hunter22").await;
    assert!(matches!(old, ServerToViewer::Error(_)));
    let (_, new) = viewer(addr, &id, "newpass22").await;
    assert!(matches!(new, ServerToViewer::AwaitingConsent { .. }));
}

#[tokio::test]
async fn rejects_untrusted_hash_params() {
    use argon2::{Algorithm, Params, Version};
    let addr = start(Config::default()).await;
    let costly = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(8, 100, 1, None).unwrap(),
    )
    .hash_password(b"pw", &SaltString::generate(&mut OsRng))
    .unwrap()
    .to_string();
    let mut ws = connect(addr, "/ws/agent").await;
    send(
        &mut ws,
        &AgentToServer::Register {
            protocol_version: PROTOCOL_VERSION,
            device_id: None,
            device_token: None,
            password_hash: costly,
            info: info(),
        },
    )
    .await;
    assert!(matches!(
        recv::<ServerToAgent>(&mut ws).await,
        ServerToAgent::Error(ErrorBody {
            code: ErrorCode::BadRequest,
            ..
        })
    ));
}

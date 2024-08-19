use std::{
    collections::HashMap,
    fmt::Write,
    io::{Cursor, Read},
    net::{Ipv4Addr, SocketAddr},
    ptr::null_mut,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use clap::Parser;
use color_eyre::owo_colors::OwoColorize;
use http_body_util::Full;
use hyper::{
    body::{Body, Incoming},
    server::conn::http1,
    service::service_fn,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use log::{error, info, trace};
use steamworks::{
    networking_sockets::NetworkingSockets,
    networking_types::{NetworkingIdentity, SendFlags},
    Client, Manager, Server, ServerManager, SteamId,
};
use steamworks_sys::{
    SteamAPI_ISteamNetworkingSockets_ConnectP2P, SteamAPI_SteamNetworkingSockets_SteamAPI_v012,
    SteamNetworkingIdentity,
};
use tokio::{
    io::{join, AsyncReadExt, AsyncWriteExt},
    join,
    net::{TcpListener, UdpSocket},
    sync::{Mutex, RwLock},
    task,
};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    address: String,
}

#[derive(Debug)]
struct State {
    servers: RwLock<HashMap<u64, Mutex<KnownServer>>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    color_eyre::install()?;

    env_logger::builder()
        .filter_level(log::LevelFilter::Trace)
        .parse_default_env()
        .init();
    let args = Args::parse();

    let state = Arc::new(State {
        servers: Default::default(),
    });

    info!("{} {}", "Listening on".bright_red(), args.address.blue());

    // let (steam, single) = Client::init()?;
    // steam.networking_utils().init_relay_network_access();
    // let s = steam
    //     .networking_sockets()
    //     .connect_p2p(
    //         NetworkingIdentity::new_steam_id(SteamId::from_raw(90200687559352326)),
    //         0,
    //         None,
    //     )
    //     .unwrap();

    let address1 = args.address.clone();
    let state1 = state.clone();

    // TODO: listen to both ipv6 and ipv4
    let listener = async move {
        let socket = Arc::new(UdpSocket::bind(address1).await.unwrap());
        loop {
            let mut buf = vec![0; 4096];
            let (n, peer) = socket.recv_from(&mut buf).await.unwrap();
            let state = state1.clone();

            task::spawn(async move {
                parse_packet(&state, buf[..n].to_vec().into(), peer).await;
            });
        }
    };

    let state2 = state.clone();
    let address2 = args.address.clone();
    let http = async move {
        let listener = TcpListener::bind(address2).await.unwrap();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let state = state2.clone();
            task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            let state = state.clone();
                            serve_http(req, state)
                        }),
                    )
                    .await
                {
                    println!("Error serving connection: {:?}", err);
                }
            });
        }
    };

    join!(http, listener);

    Ok(())
}

// reads until the next \0
fn get_str(cur: &mut Bytes) -> String {
    let npos = cur.iter().position(|&b| b == 0).unwrap();
    let res = std::str::from_utf8(&cur[..npos]).unwrap().to_string();
    cur.advance(npos + 1);
    res
}

async fn serve_http(
    r: Request<Incoming>,
    state: Arc<State>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let mut buf = BytesMut::with_capacity(4096);

    let servers = state.servers.read().await;
    // do some filtering here based on query params

    buf.put_u16(servers.len() as u16);

    for (id, server) in servers.iter() {
        let s = server.lock().await;
        buf.put_u64(*id);
        buf.write_str(&s.info).unwrap();
        buf.put_u8(0);
    }

    return Ok(Response::new(Full::new(buf.into())));
}

async fn parse_packet(state: &State, mut msg: Bytes, peer: SocketAddr) {
    dbg!(&msg);
    if msg.remaining() < 8 {
        trace!("short message from {}", peer);
        return;
    }

    // magic bytes
    if get_str(&mut msg) != "warfork" {
        return;
    }

    match msg.get_u8() {
        // heartbeat
        1 => {
            trace!("incoming heartbeat from {}", peer);

            let steamid = msg.get_u64();
            let info = get_str(&mut msg);

            let servers = state.servers.read().await;

            match servers.get(&steamid) {
                Some(smx) => {
                    let mut s = smx.lock().await;
                    s.last_heartbeat = SystemTime::now();
                    s.info = info;
                }
                None => {
                    drop(servers);

                    let entry = KnownServer {
                        owner: peer.clone(),
                        last_heartbeat: SystemTime::now(),
                        info,
                    };
                    let mut servers = state.servers.write().await;
                    servers.insert(steamid, Mutex::new(entry));

                    // TODO spawn a thread here to periodically connect to it and check
                    // the last heartbeat time
                }
            };
        }

        // close notify
        2 => {
            let steamid = msg.get_u64();
            let mut servers = state.servers.write().await;

            if let Some(smx) = servers.get(&steamid) {
                let server = smx.lock().await;
                if server.owner != peer {
                    trace!("{} tried to remove a server they didn't create!", peer);
                    return;
                }

                info!("dropping server {} owned by {}", steamid, peer);
                drop(server);
                servers.remove(&steamid);
            } else {
                trace!("{} tried to remove a server that doesn't exist", peer);
            }
        }
        _ => {}
    }
}
#[derive(Debug)]
struct KnownServer {
    owner: SocketAddr,
    last_heartbeat: SystemTime,
    info: String,
}

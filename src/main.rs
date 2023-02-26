use tokio::sync::broadcast;

use plugin::{grpc_broker_server::GrpcBrokerServer, grpc_controller_server::GrpcControllerServer, grpc_stdio_server::GrpcStdioServer};
use plugin::{GrpcIo, GrpcStdio, GrpcController, GrpcBroker};
use provider::tf::provider_server::ProviderServer;
use provider::CmdProvider;

use std::{env, fs::File, io::SeekFrom, sync::Mutex};

use anyhow::{anyhow, Result};
use futures::{try_join, TryFutureExt};
use rcgen::{BasicConstraints, IsCa};
use rustls::{
    internal::pemfile, ClientCertVerified, HandshakeSignatureValid, ProtocolVersion, TLSError,
};
use tokio::io::AsyncSeekExt;
use tonic::transport::{Server, server::ServerTlsConfig};
use tower_http::trace::TraceLayer;

use rustls::internal::msgs::handshake::DigitallySignedStruct;

const CORE_PROTOCOL_VERSION: u8 = 1;

struct CertVerifier {
    pub cert: Vec<u8>,
    pub root_store: rustls::RootCertStore,
}

impl rustls::ClientCertVerifier for CertVerifier {
    fn client_auth_root_subjects(
        &self,
        _sni: Option<&webpki::DNSName>,
    ) -> Option<rustls::DistinguishedNames> {
        Some(self.root_store.get_subjects())
    }

    fn verify_client_cert(
        &self,
        presented_certs: &[rustls::Certificate],
        _sni: Option<&webpki::DNSName>,
    ) -> Result<rustls::ClientCertVerified, TLSError> {
        if presented_certs.len() != 1 {
            return Err(TLSError::General(format!(
                "server sent {} certificates, expected one",
                presented_certs.len()
            )));
        }
        if presented_certs[0].0 != self.cert {
            return Err(TLSError::General(
                "server certificates doesn't match ours".to_string(),
            ));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::Certificate,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TLSError> {
        // It's a SHA-512 ECDSA, which webpki doesn't support. We assume by default that if the client cert
        // someone handed us equals the one in the environment variables that this is probably ok.
        //
        // FIXME: Blocked by upstream https://github.com/briansmith/ring/issues/824

        Ok(HandshakeSignatureValid::assertion())
    }
}

mod plugin;
mod provider;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_file = File::create("cmd-trace.log")?;
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(Mutex::new(log_file))
        .init();

    let addr = "0.0.0.0:10000".parse()?;
    let (tx, _) = broadcast::channel(10);
    let grpc_io = GrpcIo{tx: tx.clone()};

    let grpc_broker = GrpcBroker{io: grpc_io.clone()};
    let grpc_controller = GrpcController{io:grpc_io};
    let grpc_stdio = GrpcStdio{tx: tx};
    let provider = CmdProvider{};

    let mut client_root_cert_store = rustls::RootCertStore::empty();

    let env_cert = env::var("PLUGIN_CLIENT_CERT").unwrap();
    let mut pem_buffer = std::io::Cursor::new(env_cert.clone());
    client_root_cert_store
        .add_pem_file(&mut pem_buffer)
        .unwrap();
    let mut cp = rcgen::CertificateParams::new(vec!["localhost".to_string()]);
    cp.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let server_cert = rcgen::Certificate::from_params(cp)?;

    let mut cert_buffer = std::io::Cursor::new(server_cert.serialize_pem()?);
    let tls_cert = pemfile::certs(&mut cert_buffer).unwrap();

    let mut key_buffer = std::io::Cursor::new(server_cert.serialize_private_key_pem());
    let mut key = pemfile::pkcs8_private_keys(&mut key_buffer).unwrap();

    cert_buffer.seek(SeekFrom::Start(0)).await?;

    let raw_cert = env_cert.as_bytes();
    let x509_cert = x509_parser::pem::parse_x509_pem(raw_cert)
        .unwrap()
        .1
        .clone();
    let mut server_config = rustls::ServerConfig::new(std::sync::Arc::new(CertVerifier {
        cert: x509_cert.contents,
        root_store: client_root_cert_store,
    }));
    server_config.set_single_cert(tls_cert, key.pop().unwrap())?;
    server_config.versions = vec![ProtocolVersion::TLSv1_2];
    let mut tls_config = ServerTlsConfig::new();
    tls_config.rustls_server_config(server_config);

    let serve = Server::builder()
        .tls_config(tls_config)?
        .layer(TraceLayer::new_for_grpc())
        .add_service(GrpcBrokerServer::new(grpc_broker))
        .add_service(GrpcControllerServer::new(grpc_controller))
        .add_service(GrpcStdioServer::new(grpc_stdio))
        .add_service(ProviderServer::new(provider))
        .serve(addr);


    async fn info(server_cert: rcgen::Certificate) -> Result<()> {
        println!(
            "{}|6|tcp|localhost:10000|grpc|{}",
            CORE_PROTOCOL_VERSION,
            base64::encode_config(
                server_cert.serialize_der()?,
                base64::STANDARD_NO_PAD
                )
            );
        Ok(())
    }

    try_join!(serve.map_err(|e| anyhow!(e)), info(server_cert))?;

    Ok(())
}

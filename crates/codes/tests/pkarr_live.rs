//! Codes through the real pkarr server (dns.iroh.link). Needs internet, so the test
//! is run by hand: `cargo test -p phase-codes --features pkarr -- --ignored`.
#![cfg(feature = "pkarr")]

use iroh_base::{EndpointAddr, SecretKey, TransportAddr};
use iroh_tickets::endpoint::EndpointTicket;
use phase_codes::{client::CodesError, pkarr::PkarrCodes};

#[tokio::test]
#[ignore = "needs internet"]
async fn register_lookup_release_through_the_public_relay() {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).unwrap();
    let host = SecretKey::from_bytes(&seed).public();
    let addr = EndpointAddr::from_parts(
        host,
        [TransportAddr::Relay(
            "https://euc1-1.relay.n0.iroh.link./".parse().unwrap(),
        )],
    );
    let ticket = EndpointTicket::new(addr.clone()).to_string();

    let codes = PkarrCodes::from_env().unwrap();
    let code = codes.register(&ticket).await.unwrap();
    println!("code {code}");
    let found = codes.lookup(&code).await.unwrap();
    assert_eq!(
        EndpointTicket::from_str_checked(&found).endpoint_addr(),
        &addr
    );

    codes.release(&code).await.unwrap();
    assert!(matches!(
        codes.lookup(&code).await,
        Err(CodesError::NotFound)
    ));
}

trait Checked {
    fn from_str_checked(s: &str) -> Self;
}
impl Checked for EndpointTicket {
    fn from_str_checked(s: &str) -> Self {
        s.parse().unwrap()
    }
}

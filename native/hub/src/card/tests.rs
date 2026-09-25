#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rshell_m0::russh::keys::signature::Verifier;
use rshell_m0::russh::keys::ssh_encoding::Decode as _;
use rshell_m0::russh::keys::ssh_key::Signature;
use rshell_m0::rshell_session::ExternalSigner as _;
use secrecy::SecretString;
use tokio::sync::mpsc;

use super::virtual_card::{CARDHOLDER, IDENT, PIN, VirtualCard};
use super::{CardContext, CardFailure, CardRequest, CardSigner};

fn cards(card: &VirtualCard) -> Arc<CardContext> {
    Arc::new(CardContext::new(Arc::new(card.clone())))
}

fn pin(value: &str) -> SecretString {
    SecretString::from(value.to_owned())
}

#[test]
fn scanning_reads_the_authentication_key_and_pin_state() {
    let card = VirtualCard::new([7; 32], false);
    let found = cards(&card).scan(false).expect("scan");
    assert_eq!(found.len(), 1);
    let info = &found[0];
    assert_eq!(info.ident, IDENT);
    assert_eq!(info.cardholder, CARDHOLDER);
    assert_eq!(info.algorithm, "Ed25519");
    assert_eq!(
        info.public_key.as_ref().map(|key| key.key_data().clone()),
        Some(card.public_key().key_data().clone())
    );
    assert_eq!(info.pin_tries_left, 3);
    assert!(!info.touch);
}

#[test]
fn the_card_signs_after_the_pin_and_the_signature_verifies() {
    let card = VirtualCard::new([7; 32], true);
    let cards = cards(&card);
    let key = card.public_key();
    let data = b"session identifier and userauth request";
    assert_eq!(
        cards.sign(IDENT, &key, pin("000000"), data, None, false, &|_| {}),
        Err(CardFailure::PinWrong { tries_left: 2 })
    );

    let touched = AtomicBool::new(false);
    let blob = cards
        .sign(IDENT, &key, pin(PIN), data, None, false, &|touch| {
            touched.store(touch, Ordering::SeqCst);
        })
        .expect("signature");
    assert!(touched.load(Ordering::SeqCst), "the card asks for a touch");
    let signature = Signature::decode(&mut blob.as_slice()).expect("SSH signature blob");
    Verifier::verify(&key, data, &signature).expect("valid Ed25519 signature");
    assert_eq!(card.tries_left(), 3, "a correct PIN resets the counter");
}

#[test]
fn three_wrong_pins_block_the_card() {
    let card = VirtualCard::new([7; 32], false);
    let cards = cards(&card);
    let key = card.public_key();
    for (attempt, expected) in [
        ("1111", CardFailure::PinWrong { tries_left: 2 }),
        ("2222", CardFailure::PinWrong { tries_left: 1 }),
        ("3333", CardFailure::PinBlocked),
        (PIN, CardFailure::PinBlocked),
    ] {
        assert_eq!(
            cards.sign(IDENT, &key, pin(attempt), b"x", None, false, &|_| {}),
            Err(expected)
        );
    }
    assert_eq!(cards.probe(IDENT, &key).expect("probe").pin_tries_left, 0);
}

#[test]
fn another_key_or_another_card_is_refused() {
    let card = VirtualCard::new([7; 32], false);
    let cards = cards(&card);
    let other = VirtualCard::new([8; 32], false).public_key();
    assert_eq!(
        cards.probe(IDENT, &other).err(),
        Some(CardFailure::KeyMismatch)
    );
    assert_eq!(
        cards.sign(IDENT, &other, pin(PIN), b"x", None, false, &|_| {}),
        Err(CardFailure::KeyMismatch)
    );
    assert_eq!(
        cards.probe("0006:12345678", &card.public_key()).err(),
        Some(CardFailure::NotFound)
    );
    assert_eq!(card.tries_left(), 3, "a mismatched key never reaches VERIFY");
}

#[tokio::test]
async fn the_signer_asks_again_after_a_wrong_pin_and_remembers_on_request() {
    let card = VirtualCard::new([7; 32], false);
    let cards = cards(&card);
    let (requests, mut questions) = mpsc::unbounded_channel();
    let signer = CardSigner::new(
        cards.clone(),
        1,
        "probe@test:22".to_owned(),
        "laptop card".to_owned(),
        IDENT.to_owned(),
        card.public_key(),
        requests,
    );
    let answers = tokio::spawn(async move {
        let mut asked = Vec::new();
        for answer in ["000000", PIN] {
            let Some(CardRequest::Pin { question, reply }) = questions.recv().await else {
                return asked;
            };
            asked.push((question.key_name, question.tries_left, question.retry));
            let _ = reply.send(Some((pin(answer), true)));
        }
        asked
    });
    let blob = signer.sign(b"data", None).await.expect("signed");
    assert!(!blob.is_empty());
    assert_eq!(
        answers.await.expect("answers"),
        vec![
            ("laptop card".to_owned(), Some(3), false),
            ("laptop card".to_owned(), Some(2), true),
        ]
    );

    // 记住了 PIN：再签名不问。
    let (requests, mut questions) = mpsc::unbounded_channel();
    let again = CardSigner::new(
        cards.clone(),
        2,
        String::new(),
        String::new(),
        IDENT.to_owned(),
        card.public_key(),
        requests,
    );
    again.sign(b"data", None).await.expect("signed with the remembered PIN");
    assert!(questions.try_recv().is_err());
}

#[tokio::test]
async fn cancelling_the_pin_or_a_missing_card_is_reported_by_the_signer() {
    let card = VirtualCard::new([7; 32], false);
    let (requests, mut questions) = mpsc::unbounded_channel();
    let signer = CardSigner::new(
        cards(&card),
        1,
        String::new(),
        String::new(),
        IDENT.to_owned(),
        card.public_key(),
        requests,
    );
    tokio::spawn(async move {
        if let Some(CardRequest::Pin { reply, .. }) = questions.recv().await {
            let _ = reply.send(None);
        }
    });
    assert!(signer.sign(b"data", None).await.is_err());
    assert_eq!(signer.failure(), Some(CardFailure::Cancelled));

    let (requests, _questions) = mpsc::unbounded_channel();
    let elsewhere = CardSigner::new(
        cards(&VirtualCard::new([9; 32], false)),
        1,
        String::new(),
        String::new(),
        "0006:12345678".to_owned(),
        card.public_key(),
        requests,
    );
    assert!(elsewhere.sign(b"data", None).await.is_err());
    assert_eq!(elsewhere.failure(), Some(CardFailure::NotFound));
}

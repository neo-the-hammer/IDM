//! Every algorithm checked against its specification's published vectors.
//! These implementations are hand-written, so this file is the safety net.

use hdm_crypto::*;

// ------------------------------------------------------------------ MD5
// RFC 1321, appendix A.5 test suite.

#[test]
fn md5_rfc1321_suite() {
    for (input, want) in [
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("a", "0cc175b9c0f1b6a831c399e269772661"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
        (
            "abcdefghijklmnopqrstuvwxyz",
            "c3fcd3d76192e4007dfb496cca67e13b",
        ),
        (
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            "d174ab98d277d9f5a5611c2c9f419d9f",
        ),
        (
            "123456789012345678901234567890123456789012345678901234567890123456789\
             01234567890",
            "57edf4a22be3c955ac49da2e2107b67a",
        ),
    ] {
        assert_eq!(Md5::hex_digest(input.as_bytes()), want, "md5({input:?})");
    }
}

// ------------------------------------------------------------------ SHA-1
// RFC 3174 / FIPS 180-1.

#[test]
fn sha1_published_vectors() {
    for (input, want) in [
        ("", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        ("abc", "a9993e364706816aba3e25717850c26c9cd0d89d"),
        (
            "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
        ),
    ] {
        assert_eq!(Sha1::hex_digest(input.as_bytes()), want, "sha1({input:?})");
    }
    // One million 'a' — exercises many thousands of blocks.
    let mut h = Sha1::new();
    for _ in 0..1000 {
        h.update(&[b'a'; 1000]);
    }
    assert_eq!(hex(&h.finish()), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
}

// ------------------------------------------------------------------ SHA-256
// FIPS 180-4.

#[test]
fn sha256_published_vectors() {
    for (input, want) in [
        (
            "",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
             hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        ),
    ] {
        assert_eq!(
            Sha256::hex_digest(input.as_bytes()),
            want,
            "sha256({input:?})"
        );
    }
    let mut h = Sha256::new();
    for _ in 0..1000 {
        h.update(&[b'a'; 1000]);
    }
    assert_eq!(
        hex(&h.finish()),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

// ------------------------------------------------- block-boundary behaviour

/// Downloads are hashed in whatever chunk sizes the socket delivers, so a
/// streamed digest must equal the one-shot digest for every possible split.
#[test]
fn streaming_matches_one_shot_at_every_boundary() {
    let data: Vec<u8> = (0..600u32).map(|i| (i * 7 % 251) as u8).collect();
    let want = Sha256::hex_digest(&data);
    for split in 0..data.len() {
        let mut h = Sha256::new();
        h.update(&data[..split]);
        h.update(&data[split..]);
        assert_eq!(hex(&h.finish()), want, "split at {split}");
    }
}

/// Lengths around the 56/64-byte padding boundary are where length-suffix bugs
/// hide, in all three algorithms.
#[test]
fn padding_boundaries_are_correct() {
    for len in [0, 1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128, 129] {
        let data = vec![b'x'; len];
        // Feeding one byte at a time must agree with the one-shot digest.
        for (name, one_shot, streamed) in [
            ("md5", Md5::hex_digest(&data), {
                let mut h = Md5::new();
                data.iter().for_each(|b| h.update(&[*b]));
                hex(&h.finish())
            }),
            ("sha1", Sha1::hex_digest(&data), {
                let mut h = Sha1::new();
                data.iter().for_each(|b| h.update(&[*b]));
                hex(&h.finish())
            }),
            ("sha256", Sha256::hex_digest(&data), {
                let mut h = Sha256::new();
                data.iter().for_each(|b| h.update(&[*b]));
                hex(&h.finish())
            }),
        ] {
            assert_eq!(one_shot, streamed, "{name} at length {len}");
        }
    }
}

// ------------------------------------------------------------------ HMAC

#[test]
fn hmac_sha256_rfc4231() {
    // Case 1
    assert_eq!(
        hex(&hmac::<Sha256>(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    // Case 2 — short ASCII key
    assert_eq!(
        hex(&hmac::<Sha256>(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // Case 3 — key and data both a full block or more
    assert_eq!(
        hex(&hmac::<Sha256>(&[0xaa; 20], &[0xdd; 50])),
        "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
    );
    // Case 6 — key longer than the block size, so it gets hashed first
    assert_eq!(
        hex(&hmac::<Sha256>(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

#[test]
fn hmac_sha1_and_md5_rfc2202() {
    assert_eq!(
        hex(&hmac::<Sha1>(&[0x0b; 20], b"Hi There")),
        "b617318655057264e28bc0b6fb378c8ef146be00"
    );
    assert_eq!(
        hex(&hmac::<Md5>(&[0x0b; 16], b"Hi There")),
        "9294727a3638bb1c13f48ef8158bfc9d"
    );
    assert_eq!(
        hex(&hmac::<Md5>(b"Jefe", b"what do ya want for nothing?")),
        "750c783e6ab0b503eaa86e310a5db738"
    );
}

// ------------------------------------------------------------------ base64
// RFC 4648 section 10.

#[test]
fn base64_rfc4648_vectors() {
    for (raw, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64_encode(raw.as_bytes()), encoded, "encode({raw:?})");
        assert_eq!(
            base64_decode(encoded).unwrap(),
            raw.as_bytes(),
            "decode({encoded:?})"
        );
    }
}

#[test]
fn base64_round_trips_binary() {
    for len in 0..200 {
        let data: Vec<u8> = (0..len).map(|i| (i * 31 % 256) as u8).collect();
        assert_eq!(
            base64_decode(&base64_encode(&data)).unwrap(),
            data,
            "len {len}"
        );
        // URL-safe output is unpadded and must avoid + and /.
        let url = encode_url_safe(&data);
        assert!(!url.contains('+') && !url.contains('/') && !url.contains('='));
        assert_eq!(base64_decode(&url).unwrap(), data, "url-safe len {len}");
    }
}

#[test]
fn base64_rejects_invalid_input() {
    assert!(base64_decode("Zg!=").is_none(), "invalid character");
    assert!(
        base64_decode("A").is_none(),
        "a lone 6-bit group is not a byte"
    );
    assert!(
        base64_decode("Zm9v\nYmFy").is_some(),
        "newlines are skipped"
    );
}

/// The WebSocket opening handshake from RFC 6455 section 1.3 — SHA-1 and
/// base64 composed exactly as the API server will use them.
#[test]
fn websocket_accept_key_matches_rfc6455() {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let accept = base64_encode(&Sha1::digest(
        format!("dGhlIHNhbXBsZSBub25jZQ=={GUID}").as_bytes(),
    ));
    assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

// ------------------------------------------------------------ hex helpers

#[test]
fn hex_round_trips_and_rejects_garbage() {
    assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xa5]), "000fffa5");
    assert_eq!(unhex("000fffa5").unwrap(), vec![0x00, 0x0f, 0xff, 0xa5]);
    assert_eq!(
        unhex("000FFFA5").unwrap(),
        vec![0x00, 0x0f, 0xff, 0xa5],
        "uppercase"
    );
    assert!(unhex("abc").is_none(), "odd length");
    assert!(unhex("zz").is_none(), "non-hex");
}

// ---------------------------------------------------- constant-time compare

#[test]
fn constant_time_eq_behaves_like_equality() {
    assert!(constant_time_eq(b"", b""));
    assert!(constant_time_eq(b"token", b"token"));
    assert!(!constant_time_eq(b"token", b"tokeN"), "differs at the end");
    assert!(
        !constant_time_eq(b"token", b"Token"),
        "differs at the start"
    );
    assert!(!constant_time_eq(b"token", b"token1"), "differing lengths");
}

// ------------------------------------------------------------------ CSPRNG

#[test]
fn random_bytes_are_available_and_not_degenerate() {
    let a = random_bytes(32).expect("OS CSPRNG must be available");
    let b = random_bytes(32).expect("OS CSPRNG must be available");
    assert_eq!(a.len(), 32);
    assert_ne!(a, b, "two 256-bit draws must not collide");
    assert!(
        a.iter().any(|&x| x != a[0]),
        "output must not be a constant run"
    );
    assert!(random_bytes(0).unwrap().is_empty());
}

#[test]
fn random_tokens_are_url_safe_and_unique() {
    let tokens: Vec<String> = (0..64).map(|_| random_token(32).unwrap()).collect();
    for t in &tokens {
        assert!(
            t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token must be URL-safe: {t}"
        );
        assert!(t.len() >= 43, "32 bytes must survive encoding: {t}");
    }
    let mut sorted = tokens.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), tokens.len(), "tokens must be unique");
}

// ------------------------------------------------------- checksum plumbing

#[test]
fn hash_algo_detection() {
    assert_eq!(HashAlgo::parse("SHA-256"), Some(HashAlgo::Sha256));
    assert_eq!(HashAlgo::parse("sha_1"), Some(HashAlgo::Sha1));
    assert_eq!(HashAlgo::parse("MD5"), Some(HashAlgo::Md5));
    assert_eq!(HashAlgo::parse("crc32"), None);

    // Users usually paste a bare digest with no algorithm named.
    assert_eq!(
        HashAlgo::from_hex_len(&Md5::hex_digest(b"x")),
        Some(HashAlgo::Md5)
    );
    assert_eq!(
        HashAlgo::from_hex_len(&Sha1::hex_digest(b"x")),
        Some(HashAlgo::Sha1)
    );
    assert_eq!(
        HashAlgo::from_hex_len(&Sha256::hex_digest(b"x")),
        Some(HashAlgo::Sha256)
    );
    assert_eq!(HashAlgo::from_hex_len("abcd"), None);
}

#[test]
fn any_hasher_matches_its_concrete_algorithm() {
    let data = b"the quick brown fox";
    for (algo, want) in [
        (HashAlgo::Md5, Md5::hex_digest(data)),
        (HashAlgo::Sha1, Sha1::hex_digest(data)),
        (HashAlgo::Sha256, Sha256::hex_digest(data)),
    ] {
        let mut h = AnyHasher::new(algo);
        // Fed in pieces, the way the engine hashes a download.
        h.update(&data[..4]);
        h.update(&data[4..]);
        assert_eq!(h.hex(), want, "{}", algo.name());
    }
}

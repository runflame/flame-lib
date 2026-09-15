use super::*;

fn root() -> SpendKey {
    SpendKey::from_seed(&[0x42; 64]).unwrap()
}

#[test]
fn bip39_seed_matches_bitcoin_and_normalizes_unicode() {
    let mnemonic = Mnemonic::from_entropy(&[0; 16]).unwrap();
    assert_eq!(mnemonic.to_string(), "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about");
    assert_eq!(
        hex::encode(mnemonic.to_seed("TREZOR")),
        concat!(
            "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f",
            "09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04"
        )
    );
    assert_eq!(
        hex::encode(mnemonic.to_seed("")),
        concat!(
            "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc1",
            "9a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4"
        )
    );
    assert_eq!(mnemonic.to_seed("\u{e9}"), mnemonic.to_seed("e\u{301}"));
    assert_ne!(mnemonic.to_seed("pass"), mnemonic.to_seed("pass "));
    assert_ne!(mnemonic.to_seed("pass"), mnemonic.to_seed("PASS"));
    assert_eq!(
        SpendKey::from_mnemonic(&mnemonic, "TREZOR").unwrap(),
        SpendKey::from_seed(&mnemonic.to_seed("TREZOR")).unwrap()
    );
    let japanese = Mnemonic::from_entropy_in(Language::Japanese, &[0; 16]).unwrap();
    assert_eq!(
        Mnemonic::parse_in(
            Language::Japanese,
            japanese.to_string().replace(' ', "\u{3000}")
        )
        .unwrap(),
        japanese
    );
    assert!(Mnemonic::parse("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon").is_err());
    let unchecked = Mnemonic::parse_in_normalized_without_checksum_check(
        Language::English,
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon",
    ).unwrap();
    assert_eq!(
        SpendKey::from_mnemonic(&unchecked, ""),
        Err(Error::InvalidMnemonic)
    );
}

#[test]
fn normal_derivation_commutes_at_every_depth_and_after_hardening() {
    for starting_key in [root(), root().derive_child(HARDENED).unwrap()] {
        let mut spend = starting_key;
        let mut view = spend.to_view();
        let mut recv = view.to_recv();
        for index in [0, 1, HARDENED - 1, 256, 0x0102_0304] {
            spend = spend.derive_child(index).unwrap();
            view = view.derive_child(index).unwrap();
            recv = recv.derive_child(index).unwrap();
            assert_eq!(spend.to_view(), view);
            assert_eq!(view.to_recv(), recv);
            let address = recv.to_address();
            assert_eq!(*address.spending_key(), spend.spending_key() * G);
            assert_eq!(*address.viewing_key(), view.viewing_key() * G);
            assert_eq!(
                *address.to_tracking_address().spending_key(),
                *address.spending_key()
            );
        }
    }
}

#[test]
fn hardened_boundaries_and_full_parent_binding() {
    let parent = root();
    for index in [HARDENED, HARDENED + 1, u32::MAX] {
        assert!(parent.derive_child(index).is_ok());
        assert_eq!(
            parent.to_view().derive_child(index),
            Err(Error::HardenedDerivation)
        );
        assert_eq!(
            parent.to_recv().derive_child(index),
            Err(Error::HardenedDerivation)
        );
    }
    assert_ne!(
        parent.derive_child(HARDENED - 1).unwrap(),
        parent.derive_child(HARDENED).unwrap()
    );
    let changed_view = SpendKey::from_parts(parent.s, parent.v + Scalar::ONE, parent.t).unwrap();
    let changed_tracking =
        SpendKey::from_parts(parent.s, parent.v, parent.t + Scalar::ONE).unwrap();
    for index in [0, HARDENED] {
        let child = parent.derive_child(index).unwrap();
        assert_ne!(
            child.spending_key(),
            changed_view.derive_child(index).unwrap().spending_key()
        );
        assert_ne!(
            child.spending_key(),
            changed_tracking.derive_child(index).unwrap().spending_key()
        );
    }
}

#[test]
fn typed_encodings_round_trip_and_reject_other_capabilities() {
    let spend = root();
    let view = spend.to_view();
    let recv = view.to_recv();
    let address = recv.to_address();
    let tracking = address.to_tracking_address();
    macro_rules! round_trip {
        ($value:expr, $type:ty, $len:expr) => {{
            let text = $value.to_string();
            assert_eq!(text.len(), $len);
            assert_eq!(text.parse::<$type>().unwrap(), $value);
            assert_eq!(text.to_ascii_uppercase().parse::<$type>().unwrap(), $value);
            assert_eq!(<$type>::from_bytes(&$value.to_bytes()).unwrap(), $value);
        }};
    }
    round_trip!(spend, SpendKey, 166);
    round_trip!(view, ViewKey, 165);
    round_trip!(recv, RecvKey, 165);
    round_trip!(address, ReceivingAddress, 111);
    round_trip!(tracking, TrackingAddress, 60);
    assert_eq!(
        recv.to_string().parse::<SpendKey>(),
        Err(Error::InvalidEncoding)
    );
    assert_eq!(
        view.to_string().parse::<RecvKey>(),
        Err(Error::InvalidEncoding)
    );
    assert_eq!(
        address.to_string().parse::<TrackingAddress>(),
        Err(Error::InvalidEncoding)
    );
    assert_eq!(format!("{spend:?}"), "SpendKey([REDACTED])");
    assert_eq!(format!("{view:?}"), "ViewKey([REDACTED])");
    assert_eq!(format!("{recv:?}"), "RecvKey([REDACTED])");
}

#[test]
fn rejects_noncanonical_scalars_invalid_points_and_zero_keys() {
    let parent = root();
    for offset in [0, 32, 64] {
        let mut bytes = parent.to_bytes();
        bytes[offset..offset + 32].fill(0xff);
        assert_eq!(SpendKey::from_bytes(&bytes), Err(Error::InvalidScalar));
        assert_eq!(
            encoding::encode("spend", &bytes).parse::<SpendKey>(),
            Err(Error::InvalidScalar)
        );
    }
    for offset in [0, 32] {
        let mut bytes = parent.to_bytes();
        bytes[offset..offset + 32].fill(0);
        assert_eq!(SpendKey::from_bytes(&bytes), Err(Error::ZeroKey));
        let mut bytes = parent.to_recv().to_bytes();
        bytes[offset..offset + 32].fill(0);
        assert_eq!(RecvKey::from_bytes(&bytes), Err(Error::ZeroKey));
        bytes[offset..offset + 32].fill(0xff);
        assert_eq!(RecvKey::from_bytes(&bytes), Err(Error::InvalidPoint));
    }
    let mut bytes = parent.to_view().to_bytes();
    bytes[32..64].fill(0);
    assert_eq!(ViewKey::from_bytes(&bytes), Err(Error::ZeroKey));
    for bytes in [[0; 96], [0xff; 96]] {
        assert!(ViewKey::from_bytes(&bytes).is_err());
        assert!(ReceivingAddress::from_bytes(&bytes[..64]).is_err());
        assert!(TrackingAddress::from_bytes(&bytes[..32]).is_err());
    }
    let mut bytes = parent.to_bytes();
    bytes[64..].fill(0); // The derivation scalar may be zero.
    assert!(SpendKey::from_bytes(&bytes).is_ok());
    assert_eq!(
        SpendKey::from_bytes(&bytes[..95]),
        Err(Error::InvalidLength)
    );
    assert_eq!(ViewKey::from_bytes(&[]), Err(Error::InvalidLength));
    assert_eq!(RecvKey::from_bytes(&bytes[..64]), Err(Error::InvalidLength));
    assert_eq!(
        ReceivingAddress::from_bytes(&bytes),
        Err(Error::InvalidLength)
    );
    assert_eq!(
        TrackingAddress::from_bytes(&bytes[..31]),
        Err(Error::InvalidLength)
    );
}

#[test]
fn normal_child_scalar_disclosure_has_the_documented_scope() {
    let parent = root();
    let (ds, dv, _) = normal_derivation(&parent.to_recv(), 42).unwrap();
    let child = parent.derive_child(42).unwrap();
    assert_eq!(child.s - ds, parent.s);
    assert_eq!(child.v - dv, parent.v);
}

#[test]
fn reference_vectors() {
    let root = SpendKey::from_seed(&[0; 64]).unwrap();
    for (path, spend_hex, recv_hex, address) in [
        (
            &[][..],
            "4d7864baf60fae7dfc13f6ae21ca3d952effb8150d31f676aabb8385dab5ff077436bce57cafb4e2ac373fb1313d7a3d8ba3cc9dd7d37e4ddf4ed80c73ff6f0756174cc1d26976a5d1a307b2fb4197379a2f8ba40112452f9f7bc92b2d01f104",
            "58a8ba7ce9e780c32ba28ecdc159508e8afccb74596c6f29f94eff0552ba4336667506790c9021acfc302d3e6f1d0f65854a061cf87b44b51d3c885dde27da4256174cc1d26976a5d1a307b2fb4197379a2f8ba40112452f9f7bc92b2d01f104",
            "f1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvmxvagx0yxfqgdvlscz60n0r58ktp22qcw0s76yk5wnezzamcna5ssfgcs3c",
        ),
        (
            &[0][..],
            "ea4b455c0ca4612a80764577662c106e2ddda504bfe8dbbb8046bb9207eb2402e5a06f069a42ec117326f2f35b72e70cd7502bbff1076f7c707bf93b75edd60089cc4fc8023b48936cd3af9259d4732ac42d373064040470ff0b8155003a5b0c",
            "6c9d43624413b329de20094274dbeebbf43535f7ff307242c7fdd5248a0886468640e89f0cb97677ac949a3cd53710c4fd82c5bcfb16d4f45bcbc010a4884a3189cc4fc8023b48936cd3af9259d4732ac42d373064040470ff0b8155003a5b0c",
            "f1djw5xcjyzwejnh3qp9p8fklwh06r2d0hluc8ysk8lh2jfzsgsergvs8gnuxtjanh4j2f50x4xugvflvzck70k9k573duhsqs5jyy5vgdmtj30",
        ),
        (
            &[HARDENED][..],
            "d0ce5cf6dbcf65e686f93acd06ea7b3a2c3ba85c4801edfe909b9aaea0f1950db1f4e8583a3444c70f2331b85562851e50ff564ea3d962ebb717b64bb54082086b1b87cbad0acf6a556ef6e8a93e9f5d04e875ba3daadaf3f6373b9295e02502",
            "8afd6a2bb1c7bacf2b3e50255a829fcb1838d78d4d727b84fa3d16828fab702e3496bb96aaab48254a706f7e8af47860c4ad49f73fbeb602d92b22bffd9cc4726b1b87cbad0acf6a556ef6e8a93e9f5d04e875ba3daadaf3f6373b9295e02502",
            "f13t7k52a3c7av72e72qj44q5levvr34udf4e8hp8685tg9ratwqhrf94mj642kjp9ffcx7l5273uxp39df8mnl04kqtvjkg4llkwvgushdv07q",
        ),
        (
            &[HARDENED, 0x0102_0304][..],
            "17df83b0eb14baecdbeec3a28aae14f160c8e5f325811bb268182e63293a000433e008085c544e574b46edc30444b616f9e4935324e1bb2aa4146a84fdf3f70f2937614f4eab320d2c9af3c24c87c32fb1c508c85189df879a030259469eda09",
            "8a519096ebca5e78f98204887dd2a914e320d37d628ed9f4b49b21a469c0ef14b2aac2520ba5a2d4ee78796b9f8b0c563df10c96af7b26979268d5a38560ab062937614f4eab320d2c9af3c24c87c32fb1c508c85189df879a030259469eda09",
            "f13fgep9htef0837vzqjy8m54fzn3jp5mav28dna95nvs6g6wqau2t92kz2g96tgk5aeu8j6ul3vx9v003pjt277exj7fx34drs4s2kps2e0jrr",
        ),
    ] {
        let mut key = root.clone();
        for &index in path {
            key = key.derive_child(index).unwrap();
        }
        assert_eq!(hex::encode(key.to_bytes()), spend_hex);
        assert_eq!(hex::encode(key.to_recv().to_bytes()), recv_hex);
        assert_eq!(key.to_recv().to_address().to_string(), address);
    }
    assert_eq!(root.to_string(),
        "spend1f4uxfwhkp7h8mlqn76hzrj3aj5h0lwq4p5clva42hwpctk44lurhgd4uu472ld8z4smnlvf384armzarejwa05m7fh05akqvw0lk7p6kzaxvr5nfw6jargc8kta5r9ehnghchfqpzfzjl8mmey4j6q03qs4yv900");
    assert_eq!(root.to_view().to_string(),
        "view1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvm8gd4uu472ld8z4smnlvf384armzarejwa05m7fh05akqvw0lk7p6kzaxvr5nfw6jargc8kta5r9ehnghchfqpzfzjl8mmey4j6q03qszcekyq");
    assert_eq!(root.to_recv().to_string(),
        "recv1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvmxvagx0yxfqgdvlscz60n0r58ktp22qcw0s76yk5wnezzamcna5sjkzaxvr5nfw6jargc8kta5r9ehnghchfqpzfzjl8mmey4j6q03qse6tpf4");
    assert_eq!(
        root.to_recv()
            .to_address()
            .to_tracking_address()
            .to_string(),
        "c1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvmq72w85x"
    );
}

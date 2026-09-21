use super::vote_weight::{included_acquisition, params};
use super::*;

#[tokio::test]
async fn selects_acquisitions_for_each_blocks_target_height() {
    let first = included_acquisition(0x51, 98, 15, Some(3)); // Active: 100..103.
    let second = included_acquisition(0x51, 101, 25, None); // Active: 103..108.
    let other = included_acquisition(0x52, 99, 10, Some(1)); // Active: 101..102.
    let minter = first.acquisition.data().minter_p2wsh;
    let other_minter = other.acquisition.data().minter_p2wsh;
    let storage = Storage {
        acquisitions: HashMap::from([
            (
                minter,
                MinterAcquisitions {
                    is_double_signed: false,
                    acquisitions: vec![first.clone(), second.clone()],
                },
            ),
            (
                other_minter,
                MinterAcquisitions {
                    is_double_signed: true,
                    acquisitions: vec![other.clone()],
                },
            ),
        ]),
        ..Storage::default()
    };
    let params = params();
    let provider = AcquisitionProvider {
        storage: &storage,
        new_acquisitions: &[],
        params: &params,
    };
    for (target, expected) in [
        (99, vec![]),
        (100, vec![first.clone()]),
        (101, vec![first]),
        (103, vec![second]),
        (108, vec![]),
    ] {
        let active = provider.active_at(target).await.unwrap();
        assert_eq!(
            active
                .get(&minter)
                .map(|minter| minter.acquisitions.clone())
                .unwrap_or_default(),
            expected
        );
        if target == 101 {
            assert_eq!(active[&other_minter].acquisitions, vec![other.clone()]);
            assert!(active[&other_minter].is_double_signed);
        } else {
            assert!(!active.contains_key(&other_minter));
        }
    }
    assert_eq!(
        *storage.acquisition_reads.lock().unwrap(),
        vec![99, 100, 101, 103, 108]
    );
}

#[tokio::test]
async fn active_acquisition_query_propagates_storage_errors() {
    let storage = Storage {
        fail: true,
        ..Storage::default()
    };
    let params = params();
    let provider = AcquisitionProvider {
        storage: &storage,
        new_acquisitions: &[],
        params: &params,
    };
    assert_eq!(provider.active_at(100).await, Err("storage unavailable"));
}

#[tokio::test]
async fn merges_only_included_acquisitions_and_preserves_minter_status() {
    let stored = included_acquisition(0x51, 90, 10, Some(2)); // Expired, but retained by load_through.
    let added = included_acquisition(0x51, 100, 20, Some(5)); // Immature at 100.
    let future = included_acquisition(0x52, 101, 30, Some(5));
    let minter = stored.acquisition.data().minter_p2wsh;
    let other_minter = future.acquisition.data().minter_p2wsh;
    let storage = Storage {
        acquisitions: HashMap::from([(
            minter,
            MinterAcquisitions {
                is_double_signed: true,
                acquisitions: vec![stored.clone()],
            },
        )]),
        fail_double_sign: true, // An existing minter's status should not be loaded again.
        ..Storage::default()
    };
    let params = params();
    let new_acquisitions = [added.clone(), future];
    let provider = AcquisitionProvider {
        storage: &storage,
        new_acquisitions: &new_acquisitions,
        params: &params,
    };
    let through = provider.load_through(100).await.unwrap();
    assert_eq!(through[&minter].acquisitions, vec![stored, added]);
    assert!(through[&minter].is_double_signed);
    assert!(!through.contains_key(&other_minter));
    assert!(provider.active_at(100).await.unwrap().is_empty());
    assert_eq!(storage.acquisitions[&minter].acquisitions.len(), 1);
}

#[tokio::test]
async fn loads_double_sign_status_for_minters_with_only_new_acquisitions() {
    let new_acquisitions = [included_acquisition(0x51, 100, 20, Some(5))];
    let minter = new_acquisitions[0].acquisition.data().minter_p2wsh;
    let storage = Storage {
        double_signs: vec![DoubleSign {
            minter,
            target_flame_height: 1.into(),
            votes: vec![],
        }],
        ..Storage::default()
    };
    let mut params = params();
    params.acquisition_maturity = 0;
    let provider = AcquisitionProvider {
        storage: &storage,
        new_acquisitions: &new_acquisitions,
        params: &params,
    };
    for acquisitions in [
        provider.load_through(100).await.unwrap(),
        provider.active_at(100).await.unwrap(),
    ] {
        assert!(acquisitions[&minter].is_double_signed);
        assert_eq!(acquisitions[&minter].acquisitions, new_acquisitions);
    }
}

#[tokio::test]
async fn propagates_double_sign_lookup_errors() {
    let storage = Storage {
        fail_double_sign: true,
        ..Storage::default()
    };
    let params = params();
    let new_acquisitions = [included_acquisition(0x51, 100, 20, Some(5))];
    let provider = AcquisitionProvider {
        storage: &storage,
        new_acquisitions: &new_acquisitions,
        params: &params,
    };
    assert_eq!(
        provider.load_through(100).await,
        Err("double sign storage unavailable")
    );
    assert_eq!(
        provider.active_at(102).await,
        Err("double sign storage unavailable")
    );
}

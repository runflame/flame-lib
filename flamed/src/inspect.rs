//! Public VM data for RPC responses.

use std::collections::HashSet;

use flamed_rpc::{
    ActorId, ActorResult, ActorTarget, CellId, ContractId, ContractResult, DictEntry,
    InstructionView, PredicatePoint, TxEntry as RpcTxEntry, TxValue, ValueCell,
};
use flamevm::{
    ActorID, Anchor, Cell, CellDecode, CellEncode, CellEnvelope, CellError, CellRef, CellResolver,
    CellSlice, Contract, Instruction, Predicate, Scalar, Trie, TxEntry, Value,
};

/// Decodes an archived contract's public payload.
pub fn contract(id: ContractId, mut result: ContractResult) -> Result<ContractResult, CellError> {
    let bytes = &result.bytes.0;
    let mut gas = (bytes.len() as u64).saturating_mul(4);
    let mut envelope = CellEnvelope::decode(bytes, bytes.len(), &mut gas)?;
    if envelope.root() != id.0 {
        return Err(CellError::InvalidFormat);
    }
    let root = envelope.resolve(&CellRef::unresolved(envelope.root()))?;
    let (contract, decoded) = match Contract::from_cell(&root, &mut envelope) {
        Ok(contract) => {
            let decoded = render_value(contract.payload());
            (contract, decoded)
        }
        Err(CellError::MissingCell(_)) => {
            let contract = Contract::from_trusted_cell(&root, &mut envelope)?;
            let payload = CellRef::resident(contract.payload().to_cell()?);
            let decoded = available_cells(&payload, &mut envelope);
            (contract, decoded)
        }
        Err(error) => return Err(error),
    };
    result.anchor = contract.anchor.0;
    (result.decoded_payload, result.payload_error) = match decoded {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    };
    Ok(result)
}

/// Decodes the available state and code from a current actor snapshot.
pub fn actor(mut result: ActorResult) -> ActorResult {
    let decoded = result.state.as_ref().map(|bytes| {
        let mut gas = (bytes.0.len() as u64).saturating_mul(4);
        let mut envelope = CellEnvelope::decode(&bytes.0, bytes.0.len(), &mut gas)?;
        let reference = CellRef::unresolved(envelope.root());
        match envelope
            .resolve(&reference)
            .and_then(|cell| Value::from_cell(&cell, &mut envelope))
        {
            Ok(value) => render_value(&value),
            Err(CellError::MissingCell(_)) => available_cells(&reference, &mut envelope),
            Err(error) => Err(error),
        }
    });

    (result.decoded_state, result.state_error) = match decoded {
        Some(Ok(value)) => (Some(value), None),
        Some(Err(error)) => (None, Some(error.to_string())),
        None => (None, Some("State body is unavailable".into())),
    };

    (result.instructions, result.code_error) = match &result.code {
        Some(code) => disassemble(&code.0),
        None => (Vec::new(), Some("Code body is unavailable".into())),
    };

    result
}

fn disassemble(code: &[u8]) -> (Vec<InstructionView>, Option<String>) {
    let mut instructions = Vec::new();
    let mut remaining = code;
    while !remaining.is_empty() {
        let offset = (code.len() - remaining.len()) as u64;
        let op = match Instruction::parse(&mut remaining) {
            Ok(op) => op,
            Err(error) => {
                return (instructions, Some(format!("Byte {offset}: {error}")));
            }
        };
        let text = match op {
            Instruction::PushInt(n) => format!("push {}", scalar(n)),
            Instruction::PushStr(s) => format!("pushstr 0x{}", hex::encode(s.to_bytes())),
            Instruction::PushPoint(p) => format!("pushpoint 0x{}", hex::encode(p.to_bytes())),
            Instruction::DupK(k) => format!("dup:{k}"),
            Instruction::RollK(k) => format!("roll:{k}"),
            Instruction::Label(n) => format!("label:{n}"),
            Instruction::Jump(n) => format!("jump:{n}"),
            Instruction::JumpIf(n) => format!("jumpif:{n}"),
            Instruction::Ext(n) => format!("ext 0x{n:02x}"),
            Instruction::Alloc(_) => "alloc".into(),
            plain => format!("{plain:?}").to_ascii_lowercase(),
        };
        instructions.push(InstructionView { offset, text });
    }
    (instructions, None)
}

/// Reads public effects from an archived log.
pub fn effects(bytes: &[u8]) -> Result<Vec<RpcTxEntry>, CellError> {
    let mut gas = (bytes.len() as u64).saturating_mul(4);
    let mut envelope = CellEnvelope::decode(bytes, bytes.len(), &mut gas)?;
    let root = envelope
        .cells()
        .get(&envelope.root())
        .ok_or(CellError::MissingCell(envelope.root()))?;

    let mut slice = CellSlice::new(&root);
    let len = usize::try_from(slice.load_u64()?).map_err(|_| CellError::LimitExceeded)?;

    let trie = if len == 0 {
        Trie::new(8)?
    } else {
        Trie::from_cell(slice.load_ref()?, 8)?
    };

    slice.finish()?;

    trie.entries_exact(len, &mut envelope)?
        .into_iter()
        .enumerate()
        .map(|(i, (key, reference))| {
            if key != (i as u64).to_be_bytes() {
                return Err(CellError::InvalidFormat);
            }
            let cell = envelope.resolve(&reference)?;
            effect_from_cell(&cell, &mut envelope)
        })
        .collect()
}

fn effect_from_cell(cell: &Cell, envelope: &mut CellEnvelope) -> Result<RpcTxEntry, CellError> {
    match TxEntry::from_cell(cell, envelope) {
        Ok(entry) => return render_effect(&entry),
        Err(CellError::MissingCell(_)) => {}
        Err(error) => return Err(error),
    }

    let mut slice = CellSlice::new(cell);
    // Archived logs can omit old state cells. Keep their hashes and all available bodies.
    let effect = match slice.load_u8()? {
        TxEntry::TAG_ACTOR_SAVE => {
            let actor = ActorId(<[u8; 32]>::decode(&mut slice, envelope)?);
            let state = slice.load_ref()?;
            RpcTxEntry::ActorSave {
                actor,
                state_hash: state.id(),
                state: available_cells(&state, envelope)?,
            }
        }
        TxEntry::TAG_OUTPUT => {
            let contract = envelope.resolve(&slice.load_ref()?)?;
            let contract = Contract::from_trusted_cell(&contract, envelope)?;
            let payload = CellRef::resident(contract.payload().to_cell()?);
            RpcTxEntry::Output {
                contract: ContractId(contract.id()),
                predicate: PredicatePoint(contract.predicate.to_point().to_bytes()),
                anchor: contract.anchor.0,
                payload: available_cells(&payload, envelope)?,
            }
        }
        TxEntry::TAG_SEND => {
            let message = envelope.resolve(&slice.load_ref()?)?;
            message_effect(&message, envelope)?
        }
        _ => return Err(CellError::InvalidFormat),
    };

    slice.finish()?;
    Ok(effect)
}

fn message_effect(cell: &Cell, envelope: &mut CellEnvelope) -> Result<RpcTxEntry, CellError> {
    let mut slice = CellSlice::new(cell);
    let anchor = Anchor::decode(&mut slice, envelope)?.0;
    let target = actor_target(&ActorID::decode(&mut slice, envelope)?);
    let caller = match slice.load_u8()? {
        0 => None,
        1 => Some(ActorId(<[u8; 32]>::decode(&mut slice, envelope)?)),
        _ => return Err(CellError::InvalidFormat),
    };

    let refund_predicate = PredicatePoint(
        Predicate::decode(&mut slice, envelope)?
            .to_point()
            .to_bytes(),
    );

    let gas_limit = slice.load_u64()?.to_string();
    let payload = envelope.resolve(&slice.load_ref()?)?;
    slice.finish()?;

    // Message arguments use a dictionary with consecutive keys starting at zero.
    let mut slice = CellSlice::new(&payload);
    let len = usize::try_from(slice.load_u64()?).map_err(|_| CellError::LimitExceeded)?;
    if slice.load_u8()? & !3 != 0 {
        return Err(CellError::InvalidFormat);
    }

    let trie = if len == 0 {
        Trie::new(32)?
    } else {
        Trie::from_cell(slice.load_ref()?, 32)?
    };

    slice.finish()?;

    let payload = trie
        .entries_exact(len, envelope)?
        .into_iter()
        .enumerate()
        .map(|(i, (key, reference))| {
            let mut expected = Scalar::from(i as u64).to_bytes();
            expected.reverse();
            if key != expected {
                return Err(CellError::InvalidFormat);
            }
            match envelope
                .resolve(&reference)
                .and_then(|cell| Value::from_cell(&cell, envelope))
            {
                Ok(value) => render_value(&value),
                Err(CellError::MissingCell(_)) => available_cells(&reference, envelope),
                Err(error) => Err(error),
            }
        })
        .collect::<Result<_, _>>()?;

    Ok(RpcTxEntry::Send {
        message: cell.id(),
        target,
        caller,
        anchor,
        payload,
        gas_limit,
        refund_predicate,
    })
}

fn scalar(n: Scalar) -> String {
    n.centered_abs()
        .to_u128()
        .map(|v| {
            if n.is_centered_negative() {
                format!("-{v}")
            } else {
                v.to_string()
            }
        })
        .unwrap_or_else(|| format!("0x{}", hex::encode(n.to_bytes())))
}

fn actor_target(actor: &ActorID) -> ActorTarget {
    let id = ActorId(actor.to_hash());
    match actor {
        ActorID::Hash(_) => ActorTarget::Hash { actor: id },
        ActorID::Constructor(code) => ActorTarget::Constructor {
            actor: id,
            code: code.clone(),
        },
    }
}

fn render_effect(entry: &TxEntry) -> Result<RpcTxEntry, CellError> {
    Ok(match entry {
        TxEntry::Header(h) => RpcTxEntry::Header {
            version: h.version,
            locktime: h.locktime,
        },
        TxEntry::CellWitness(hash) => RpcTxEntry::CellWitness { hash: *hash },
        TxEntry::Data(bytes) => RpcTxEntry::Data {
            bytes: bytes.clone(),
        },
        TxEntry::Input(id) => RpcTxEntry::Input {
            contract: ContractId(*id),
        },
        TxEntry::Receive(id) => RpcTxEntry::Receive { message: *id },
        TxEntry::Output(c) => RpcTxEntry::Output {
            contract: ContractId(c.id()),
            predicate: PredicatePoint(c.predicate.to_point().to_bytes()),
            anchor: c.anchor.0,
            payload: render_value(c.payload())?,
        },
        TxEntry::ActorDeploy { actor, code } => RpcTxEntry::ActorDeploy {
            actor: ActorId(actor.to_hash()),
            code: code.clone(),
            code_hash: flamevm::code_root(code),
        },
        TxEntry::ActorSave { actor, state } => RpcTxEntry::ActorSave {
            actor: ActorId(actor.to_hash()),
            state_hash: flamevm::state_root(state),
            state: render_value(state)?,
        },
        TxEntry::SetCode { actor, code } => RpcTxEntry::SetCode {
            actor: ActorId(actor.to_hash()),
            code: code.clone(),
            code_hash: flamevm::code_root(code),
        },
        TxEntry::ActorDestroy { actor } => RpcTxEntry::ActorDestroy {
            actor: ActorId(actor.to_hash()),
        },
        TxEntry::Send(m) => RpcTxEntry::Send {
            message: m.id().0,
            target: actor_target(&m.target),
            caller: m.caller.as_ref().map(|a| ActorId(a.to_hash())),
            anchor: m.anchor.0,
            payload: m
                .payload()
                .iter()
                .map(render_value)
                .collect::<Result<_, _>>()?,
            gas_limit: m.gas.to_string(),
            refund_predicate: PredicatePoint(m.refund_predicate.to_point().to_bytes()),
        },
        TxEntry::StoragePurchase {
            actor,
            bytes,
            expiry_height,
            fee_sparks,
        } => RpcTxEntry::StoragePurchase {
            actor: ActorId(actor.to_hash()),
            bytes: *bytes,
            expiry_height: *expiry_height,
            fee_sparks: scalar(*fee_sparks),
        },
        TxEntry::Fee(q) => RpcTxEntry::Fee {
            sparks: q.to_string(),
        },
        TxEntry::IssuePub(q, f) => RpcTxEntry::IssuePublic {
            quantity: scalar(*q),
            flavor: scalar(*f),
        },
        TxEntry::IssuePriv(q, f) => RpcTxEntry::IssuePrivate {
            quantity_commitment: q.to_bytes(),
            flavor_commitment: f.to_bytes(),
        },
        TxEntry::Retire(q, f) => RpcTxEntry::Retire {
            quantity_commitment: q.to_bytes(),
            flavor_commitment: f.to_bytes(),
        },
    })
}

fn render_value(value: &Value) -> Result<TxValue, CellError> {
    Ok(match value {
        Value::Scalar(n) => TxValue::Scalar { value: scalar(*n) },
        Value::String(s) => TxValue::String {
            bytes: s.clone().to_bytes(),
        },
        Value::Point(p) => TxValue::Point { hex: p.to_bytes() },
        Value::ClearToken(t) => TxValue::ClearToken {
            quantity: scalar(t.qty()),
            flavor: scalar(t.flv()),
        },
        Value::Token(t) => TxValue::Token {
            quantity_commitment: t.qty().to_point().to_bytes(),
            flavor_commitment: t.flv().to_point().to_bytes(),
        },
        Value::Dict(d) => TxValue::Dict {
            entries: d
                .entries()
                .map(|(key, value)| {
                    Ok(DictEntry {
                        key: scalar(*key),
                        value: render_value(value)?,
                    })
                })
                .collect::<Result<_, CellError>>()?,
        },
        _ => return Err(CellError::InvalidFormat),
    })
}

fn available_cells(reference: &CellRef, envelope: &mut CellEnvelope) -> Result<TxValue, CellError> {
    let mut pending = vec![reference.clone()];
    let mut seen = HashSet::new();
    let mut cells = Vec::new();
    while let Some(reference) = pending.pop() {
        if !seen.insert(reference.id()) {
            continue;
        }
        match envelope.resolve(&reference) {
            Ok(cell) => {
                pending.extend(cell.refs().iter().cloned());
                cells.push(ValueCell {
                    id: CellId(cell.id()),
                    data: cell.payload().to_vec(),
                    refs: cell.refs().iter().map(|r| CellId(r.id())).collect(),
                });
            }
            Err(CellError::MissingCell(_)) => {}
            Err(error) => return Err(error),
        }
    }
    cells.sort_unstable_by_key(|cell| cell.id);

    Ok(TxValue::Cells {
        root: CellId(reference.id()),
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED;
    use flamevm::{
        BagOfCells, CellBuilder, ClearToken, Dict, Message, String as VmString, TxHeader, TxLog,
    };
    use serde_json::json;

    #[test]
    fn archived_effects_preserve_available_cells_and_unloaded_references() {
        let source = Dict::from_values(vec![
            Value::Scalar(Scalar::from(42u64)),
            Value::Scalar(Scalar::from(43u64)),
        ])
        .to_cell()
        .unwrap();
        let CellRef::Resident(trie) = &source.refs()[0] else {
            panic!("expected a resident trie")
        };
        assert_eq!(trie.refs().len(), 2);
        let mut builder = CellBuilder::new();
        builder.store_bytes(trie.payload()).unwrap();
        builder
            .store_ref(trie.refs()[0].to_unloaded().unwrap())
            .unwrap();
        builder.store_ref(trie.refs()[1].clone()).unwrap();
        let partial_trie = builder.build();
        let mut builder = CellBuilder::new();
        builder.store_bytes(source.payload()).unwrap();
        builder.store_ref(CellRef::resident(partial_trie)).unwrap();
        let cell = builder.build();
        let dict = Dict::decode_trusted(&mut CellSlice::new(&cell), &mut ()).unwrap();
        let state = Value::Dict(dict);
        let state_cell = state.to_cell().unwrap();
        let expected_cells = BagOfCells::collect(state_cell.clone().into()).unwrap();
        let actor = ActorID::Hash([0x11; 32]);
        let predicate = Predicate::opaque(RISTRETTO_BASEPOINT_COMPRESSED);
        let entries = vec![
            TxEntry::ActorSave {
                actor: actor.clone(),
                state: state.clone(),
            },
            TxEntry::Output(
                Contract::new(predicate.clone(), Anchor([0x22; 32]), state.clone()).unwrap(),
            ),
            TxEntry::Send(
                Message::new(
                    actor,
                    Some(ActorID::Hash([0x33; 32])),
                    Anchor([0x44; 32]),
                    vec![state],
                    u64::MAX,
                    predicate,
                )
                .unwrap(),
            ),
        ];
        for entry in entries {
            let bytes = crate::cells::log_bytes(&TxLog::from(vec![entry])).unwrap();
            let mut gas = (bytes.len() as u64) * 4;
            let mut envelope = CellEnvelope::decode(&bytes, bytes.len(), &mut gas).unwrap();
            let root = envelope.cells().get(&envelope.root()).unwrap();
            assert!(matches!(
                TxLog::from_cell(&root, &mut envelope),
                Err(CellError::MissingCell(_))
            ));
            let decoded = effects(&bytes).unwrap();
            let value = match &decoded[0] {
                RpcTxEntry::ActorSave { state, .. } => state,
                RpcTxEntry::Output { payload, .. } => payload,
                RpcTxEntry::Send { payload, .. } => &payload[0],
                _ => panic!("expected a value"),
            };
            let TxValue::Cells { root, cells } = value else {
                panic!("expected available cells")
            };
            assert_eq!(root.0, state_cell.id());
            assert_eq!(cells.len(), expected_cells.len());
            assert!(cells.windows(2).all(|pair| pair[0].id < pair[1].id));
            for cell in cells {
                let expected = expected_cells.get(&cell.id.0).unwrap();
                assert_eq!(cell.data, expected.payload());
                assert_eq!(
                    cell.refs,
                    expected
                        .refs()
                        .iter()
                        .map(|r| CellId(r.id()))
                        .collect::<Vec<_>>()
                );
            }
            assert!(cells
                .iter()
                .any(|c| c.refs.iter().any(|r| !expected_cells.contains(&r.0))));
            let json = serde_json::to_value(&decoded).unwrap();
            assert_eq!(
                serde_json::from_value::<Vec<RpcTxEntry>>(json).unwrap(),
                decoded
            );
        }
    }

    #[test]
    fn absent_state_root_stays_a_hash_reference() {
        let state_ref = CellRef::resident(Value::Scalar(Scalar::ONE).to_cell().unwrap())
            .to_unloaded()
            .unwrap();
        let state = state_ref.id();
        let mut builder = CellBuilder::new();
        builder
            .store_u8(TxEntry::TAG_ACTOR_SAVE)
            .unwrap()
            .store_bytes(&[0x11; 32])
            .unwrap()
            .store_ref(state_ref)
            .unwrap();
        let mut trie = Trie::new(8).unwrap();
        trie.insert_ref(
            &0u64.to_be_bytes(),
            CellRef::resident(builder.build()),
            &mut (),
        )
        .unwrap();
        let mut builder = CellBuilder::new();
        builder
            .store_u64(1)
            .unwrap()
            .store_ref(trie.into_root().unwrap())
            .unwrap();
        let root = builder.build();
        let envelope =
            CellEnvelope::new(root.id(), BagOfCells::collect(root.into()).unwrap()).unwrap();
        assert_eq!(
            effects(&envelope.encode()).unwrap(),
            vec![RpcTxEntry::ActorSave {
                actor: ActorId([0x11; 32]),
                state_hash: state,
                state: TxValue::Cells {
                    root: CellId(state),
                    cells: vec![]
                },
            }]
        );
    }

    #[test]
    fn archived_effects_keep_order_precision_and_full_data() {
        let code = vec![0xcd; 257];
        let log = TxLog::from(vec![
            TxEntry::Data(vec![0xab; 257]),
            TxEntry::Fee(u64::MAX),
            TxEntry::SetCode {
                actor: ActorID::Hash([0x11; 32]),
                code: code.clone(),
            },
        ]);
        let bytes = crate::cells::log_bytes(&log).unwrap();
        let decoded = effects(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            json!([
                {
                    "kind": "data",
                    "data": { "bytes": "q6ur".repeat(85) + "q6s=" },
                },
                {
                    "kind": "fee",
                    "data": { "sparks": "18446744073709551615" },
                },
                {
                    "kind": "set_code",
                    "data": {
                        "actor": "11".repeat(32),
                        "code": "zc3N".repeat(85) + "zc0=",
                        "code_hash": hex::encode(flamevm::code_root(&code)),
                    },
                },
            ])
        );
        assert_eq!(
            serde_json::from_str::<Vec<RpcTxEntry>>(&serde_json::to_string(&decoded).unwrap())
                .unwrap(),
            decoded
        );
        assert!(effects(&bytes[..bytes.len() - 1]).is_err());
        assert!(effects(&[]).is_err());
        let empty = crate::cells::log_bytes(&TxLog::from(vec![])).unwrap();
        assert!(effects(&empty).unwrap().is_empty());
    }

    #[test]
    fn full_effects_include_payloads_state_and_message_fields() {
        let actor = ActorID::Hash([0x11; 32]);
        let point = RISTRETTO_BASEPOINT_COMPRESSED;
        let predicate = Predicate::opaque(point);
        let token = Value::ClearToken(ClearToken::new(Scalar::from(7u64), Scalar::from(8u64)));
        let dict = Value::Dict(Dict::from_values(vec![
            Value::String(VmString::Opaque(vec![0xab; 257])),
            token.clone(),
        ]));
        let target = ActorID::Constructor(vec![0xcd; 257]);
        let entries = vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: u32::MAX,
            }),
            TxEntry::CellWitness([0x22; 32]),
            TxEntry::Input([0x33; 32]),
            TxEntry::Receive([0x44; 32]),
            TxEntry::Output(
                Contract::new(predicate.clone(), Anchor([0x55; 32]), dict.clone()).unwrap(),
            ),
            TxEntry::ActorDeploy {
                actor: actor.clone(),
                code: vec![0xef; 257],
            },
            TxEntry::ActorSave {
                actor: actor.clone(),
                state: dict,
            },
            TxEntry::Send(
                Message::new(
                    target.clone(),
                    Some(actor.clone()),
                    Anchor([0x66; 32]),
                    vec![token],
                    u64::MAX,
                    predicate,
                )
                .unwrap(),
            ),
            TxEntry::StoragePurchase {
                actor: actor.clone(),
                bytes: u64::MAX,
                expiry_height: u64::MAX,
                fee_sparks: Scalar::from(u64::MAX),
            },
            TxEntry::IssuePub(Scalar::from(9u64), Scalar::from(10u64)),
            TxEntry::IssuePriv(point, point),
            TxEntry::Retire(point, point),
            TxEntry::ActorDestroy { actor },
        ];
        let expected = entries
            .iter()
            .map(render_effect)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let bytes = crate::cells::log_bytes(&TxLog::from(entries)).unwrap();
        let decoded = effects(&bytes).unwrap();
        assert_eq!(decoded, expected);
        let json = serde_json::to_value(&decoded).unwrap();
        assert_eq!(json[4]["data"]["anchor"], "55".repeat(32));
        assert_eq!(
            json[4]["data"]["payload"]["entries"][0]["value"]["bytes"],
            "q6ur".repeat(85) + "q6s="
        );
        assert_eq!(json[5]["data"]["code"], "7+/v".repeat(85) + "7+8=");
        assert_eq!(json[6]["data"]["state"], json[4]["data"]["payload"]);
        assert_eq!(
            json[7]["data"],
            json!({
                "message": json[7]["data"]["message"],
                "target": {
                    "kind": "constructor",
                    "actor": hex::encode(target.to_hash()),
                    "code": "zc3N".repeat(85) + "zc0=",
                },
                "caller": "11".repeat(32),
                "anchor": "66".repeat(32),
                "payload": [{ "type": "clear_token", "quantity": "7", "flavor": "8" }],
                "gas_limit": u64::MAX.to_string(),
                "refund_predicate": hex::encode(point.to_bytes()),
            })
        );
        assert_eq!(
            serde_json::from_value::<Vec<RpcTxEntry>>(json).unwrap(),
            decoded
        );
    }
}

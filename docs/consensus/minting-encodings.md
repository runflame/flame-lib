# Minting Bitcoin Encodings

This document defines the canonical Bitcoin script encodings used by Flame Acquisition and Minting transactions. Their
consensus meaning, lifecycle, voting rules, and reward behavior are defined in [Consensus Design](design-doc.md).

## Conventions

The following fixed-width encodings are used:

| Field            |     Size | Encoding                                            |
|------------------|---------:|-----------------------------------------------------|
| Version          |   1 byte | Unsigned 8-bit integer; the current version is `1`  |
| Minter P2WSH     | 32 bytes | SHA-256 hash of the complete Bitcoin witness script |
| Access Predicate | 32 bytes | Canonical compressed Flame predicate point          |
| Validator Pubkey | 32 bytes | Ed25519 verifying key                               |
| Duration         |  2 bytes | Unsigned 16-bit integer, little-endian              |
| Flame Height     |  4 bytes | Unsigned 32-bit integer, little-endian              |
| Flame Hash       | 32 bytes | Flame block hash bytes                              |
| Flame Predicate  | 32 bytes | Canonical compressed Flame predicate point          |

All opcodes described below are mandatory. Alternative push encodings are invalid even when they push identical bytes.
Output scripts must contain exactly the specified bytes and must not contain trailing instructions or data. The Minter
authentication encoding is a prefix of a complete witness script, so authorization logic may follow its mandatory
prefix.

## Acquisition Output

An Acquisition Output script has exactly one of the following forms:

```text
OP_RETURN OP_PUSHDATA1 101 "FLMS" <Version> <Minter P2WSH> <Access Predicate> <Validator Pubkey>

OP_RETURN OP_PUSHDATA1 103 "FLMS" <Version> <Minter P2WSH> <Access Predicate> <Validator Pubkey> <Duration>
```

The corresponding opcode and length bytes are:

```text
without Duration: 0x6a 0x4c 0x65
with Duration:    0x6a 0x4c 0x67
```

The payload layout is:

| Offset |   Size | Field                   |
|-------:|-------:|-------------------------|
|      0 |      4 | ASCII `FLMS`            |
|      4 |      1 | Version; must equal `1` |
|      5 |     32 | Minter P2WSH            |
|     37 |     32 | Access Predicate        |
|     69 |     32 | Validator Pubkey        |
|    101 | 0 or 2 | Optional Duration       |

The Bitcoin output value must be non-zero. Each valid Acquisition Output is processed independently and is identified by
its transaction ID and output index. When `Duration` is absent, the protocol-defined minimum duration is used.

## Minting Output

A Minting Output script has exactly this form:

```text
OP_RETURN OP_PUSHBYTES_41 "FLMB" <Version> <Flame Height> <Flame Hash>
```

Its opcode and length bytes are:

```text
0x6a 0x29
```

The payload layout is:

| Offset | Size | Field                               |
|-------:|-----:|-------------------------------------|
|      0 |    4 | ASCII `FLMB`                        |
|      4 |    1 | Version; must equal `1`             |
|      5 |    4 | Flame Height (`u32`, little-endian) |
|      9 |   32 | Flame Hash                          |

The Bitcoin output value may be zero or non-zero. It does not contribute Minting Power and is ignored when calculating
the vote's weight.

A Minting Transaction must contain exactly one eligible Minting Output. A transaction containing no eligible Minting
Output or more than one eligible Minting Output produces no Minting votes. An unsupported Version, `OP_PUSHDATA1`,
`OP_PUSHDATA2`, `OP_PUSHDATA4`, and all push opcodes other than the exact `OP_PUSHBYTES_41` encoding above are invalid,
as are malformed fields and trailing script bytes.

## Minter Authentication Prefix

The complete witness script of an authenticating P2WSH input must begin with exactly this prefix:

```text
OP_PUSHBYTES_36 "FLMV" <Flame Predicate> OP_DROP
```

Its byte layout is:

```text
0x24 || 0x46 0x4c 0x4d 0x56 || <32-byte Flame Predicate> || 0x75
```

`OP_PUSHBYTES_36` (`0x24`) and `OP_DROP` (`0x75`) are mandatory. `OP_PUSHDATA1`, `OP_PUSHDATA2`, `OP_PUSHDATA4`, split
pushes, or any other equivalent construction do not form a valid Minter authentication prefix.

The SHA-256 hash of the complete witness script, including all authorization logic following this prefix, must equal the
Minter P2WSH committed by the Minter's Acquisition. Bitcoin validates execution of the complete witness script; Flame
uses the prefix to recover the Flame Predicate associated with the authenticated Minter.

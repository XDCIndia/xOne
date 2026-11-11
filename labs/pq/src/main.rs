//! PQ cost lab (XA-13, issue #18). Research only: nothing here ships and `bin/xone`
//! does not depend on it (`tests/no_ship.rs`). One timing loop for every scheme:
//! keygen, sign, verify of a valid signature, verify of two kinds of invalid signature.
//! Signed message is a 32-byte digest, the same shape the chain signs today.
//! Contract: docs/perf.md. Output: one JSON object per line on stdout, raw samples to `--csv`.

use std::fmt::Write as _;
use std::hint::black_box;
use std::io::Write as _;
use std::time::{Duration, Instant};

use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{RecoveryId, Signature as EcSig, SigningKey as EcSk, VerifyingKey as EcVk};
use ml_dsa::{KeyInit, MlDsa44, MlDsa65, MlDsa87, MlDsaParams, Signer, Verifier};

const SEED: [u8; 32] = [7; 32];
const MSG: [u8; 32] = [0x11; 32];
const OTHER: [u8; 32] = [0x22; 32];
const CTX: &[u8] = b"xone-pq-lab";

/// One scheme under test. `verify` takes wire bytes, so decoding is inside the timed call.
trait Scheme {
    fn pk_len(&self) -> usize;
    fn keygen(&mut self) -> bool;
    fn sign(&mut self, m: &[u8]) -> Vec<u8>;
    fn verify(&mut self, sig: &[u8], m: &[u8]) -> bool;
    /// Public-key recovery (ECDSA only; the chain recovers, it does not verify against a key).
    fn recover(&mut self, _sig: &[u8], _m: &[u8]) -> Option<bool> {
        None
    }
}

struct Ecdsa {
    sk: EcSk,
    vk: EcVk,
}
impl Scheme for Ecdsa {
    fn pk_len(&self) -> usize {
        self.vk.to_encoded_point(true).len() // compressed SEC1
    }
    fn keygen(&mut self) -> bool {
        let sk = EcSk::from_bytes(&SEED.into()).unwrap();
        black_box(sk.verifying_key());
        true
    }
    fn sign(&mut self, m: &[u8]) -> Vec<u8> {
        let (sig, id) = self.sk.sign_prehash_recoverable(m).unwrap();
        let mut out = sig.to_bytes().to_vec();
        out.push(id.to_byte());
        out
    }
    fn verify(&mut self, sig: &[u8], m: &[u8]) -> bool {
        EcSig::from_slice(&sig[..64]).is_ok_and(|s| self.vk.verify_prehash(m, &s).is_ok())
    }
    fn recover(&mut self, sig: &[u8], m: &[u8]) -> Option<bool> {
        let ok = || {
            let s = EcSig::from_slice(&sig[..64]).ok()?;
            let id = RecoveryId::from_byte(sig[64])?;
            EcVk::recover_from_prehash(m, &s, id).ok()
        };
        Some(ok().is_some())
    }
}

struct Lattice<P: MlDsaParams> {
    sk: ml_dsa::SigningKey<P>,
    vk: ml_dsa::VerifyingKey<P>,
}
impl<P: MlDsaParams> Lattice<P> {
    fn new() -> Self {
        let sk = ml_dsa::SigningKey::<P>::new_from_slice(&SEED).unwrap();
        let vk = ml_dsa::Keypair::verifying_key(&sk);
        Self { sk, vk }
    }
}
impl<P: MlDsaParams> Scheme for Lattice<P> {
    fn pk_len(&self) -> usize {
        self.vk.encode().as_slice().len()
    }
    fn keygen(&mut self) -> bool {
        let sk = ml_dsa::SigningKey::<P>::new_from_slice(&SEED).unwrap();
        black_box(ml_dsa::Keypair::verifying_key(&sk));
        true
    }
    // The Signer impl is the deterministic FIPS 204 variant with an empty context.
    fn sign(&mut self, m: &[u8]) -> Vec<u8> {
        self.sk.sign(m).encode().as_slice().to_vec()
    }
    fn verify(&mut self, sig: &[u8], m: &[u8]) -> bool {
        let Ok(enc) = ml_dsa::EncodedSignature::<P>::try_from(sig) else {
            return false;
        };
        ml_dsa::Signature::<P>::decode(&enc).is_some_and(|s| self.vk.verify(m, &s).is_ok())
    }
}

macro_rules! fips204 {
    ($m:ident) => {{
        use fips204::traits::{KeyGen, Signer, Verifier};
        use fips204::$m as p;
        struct S(p::PublicKey, p::PrivateKey);
        impl Scheme for S {
            fn pk_len(&self) -> usize {
                p::PK_LEN
            }
            fn keygen(&mut self) -> bool {
                black_box(p::KG::keygen_from_seed(&SEED));
                true
            }
            fn sign(&mut self, m: &[u8]) -> Vec<u8> {
                self.1.try_sign(m, CTX).unwrap().to_vec() // hedged: OS randomness per call
            }
            fn verify(&mut self, sig: &[u8], m: &[u8]) -> bool {
                <[u8; p::SIG_LEN]>::try_from(sig).is_ok_and(|s| self.0.verify(m, &s, CTX))
            }
        }
        let (pk, sk) = p::KG::keygen_from_seed(&SEED);
        Box::new(S(pk, sk)) as Box<dyn Scheme>
    }};
}

macro_rules! fips205 {
    ($m:ident) => {{
        use fips205::traits::{KeyGen, Signer, Verifier};
        use fips205::$m as p;
        struct S(p::PublicKey, p::PrivateKey);
        impl Scheme for S {
            fn pk_len(&self) -> usize {
                p::PK_LEN
            }
            fn keygen(&mut self) -> bool {
                black_box(p::KG::keygen_from_seed(&[1; p::N], &[2; p::N], &[3; p::N]));
                true
            }
            fn sign(&mut self, m: &[u8]) -> Vec<u8> {
                self.1.try_sign(m, CTX, true).unwrap().to_vec() // hedged: OS randomness per call
            }
            fn verify(&mut self, sig: &[u8], m: &[u8]) -> bool {
                <[u8; p::SIG_LEN]>::try_from(sig).is_ok_and(|s| self.0.verify(m, &s, CTX))
            }
        }
        let (pk, sk) = p::KG::keygen_from_seed(&[1; p::N], &[2; p::N], &[3; p::N]);
        Box::new(S(pk, sk)) as Box<dyn Scheme>
    }};
}

/// (implementation, scheme, constructor). ECDSA is the recoverable form: `r || s || v`.
/// Constructors are lazy: SLH-DSA key generation is slow and `--only` must not pay for it.
type Ctor = fn() -> Box<dyn Scheme>;
fn schemes() -> Vec<(&'static str, &'static str, Ctor)> {
    vec![
        ("k256", "ECDSA-secp256k1", || {
            let sk = EcSk::from_bytes(&SEED.into()).unwrap();
            let vk = *sk.verifying_key();
            Box::new(Ecdsa { sk, vk })
        }),
        (
            "ml-dsa",
            "ML-DSA-44",
            || Box::new(Lattice::<MlDsa44>::new()),
        ),
        (
            "ml-dsa",
            "ML-DSA-65",
            || Box::new(Lattice::<MlDsa65>::new()),
        ),
        (
            "ml-dsa",
            "ML-DSA-87",
            || Box::new(Lattice::<MlDsa87>::new()),
        ),
        ("fips204", "ML-DSA-44", || fips204!(ml_dsa_44)),
        ("fips204", "ML-DSA-65", || fips204!(ml_dsa_65)),
        ("fips204", "ML-DSA-87", || fips204!(ml_dsa_87)),
        ("fips205", "SLH-DSA-SHAKE-128s", || {
            fips205!(slh_dsa_shake_128s)
        }),
        ("fips205", "SLH-DSA-SHAKE-128f", || {
            fips205!(slh_dsa_shake_128f)
        }),
        ("fips205", "SLH-DSA-SHAKE-192f", || {
            fips205!(slh_dsa_shake_192f)
        }),
    ]
}

struct Cfg {
    min_n: usize,
    max_n: usize,
    budget: Duration,
    warmup: usize,
}

/// Time `f` per call until `max_n` samples, or until `budget` has passed and at least `min_n`
/// exist: slow operations stop at `min_n`, where p99 is the maximum (`n` in the output says so).
/// `expect` is asserted on every call, so an "invalid" row never silently measures a valid one.
fn measure(cfg: &Cfg, expect: bool, mut f: impl FnMut() -> bool) -> Vec<u64> {
    for _ in 0..cfg.warmup {
        assert_eq!(black_box(f()), expect);
    }
    let (start, mut out) = (Instant::now(), Vec::new());
    while out.len() < cfg.max_n && (out.len() < cfg.min_n || start.elapsed() < cfg.budget) {
        let t = Instant::now();
        let r = black_box(f());
        out.push(t.elapsed().as_nanos() as u64);
        assert_eq!(r, expect);
    }
    out
}

/// Nearest rank, integer arithmetic: the same rule as bench/collect/summarise.py.
fn pct(sorted: &[u64], permille: usize) -> u64 {
    sorted[(permille * sorted.len()).div_ceil(1000).saturating_sub(1)]
}

fn flip_mid(sig: &[u8]) -> Vec<u8> {
    let mut s = sig.to_vec();
    s[sig.len() / 2] ^= 1;
    s
}

fn run(cfg: &Cfg, only: &str, csv: &mut Option<std::io::BufWriter<std::fs::File>>) {
    for (imp, scheme, ctor) in schemes() {
        if !format!("{imp}/{scheme}").contains(only) {
            continue;
        }
        let mut s = ctor();
        let (pk, sig) = (s.pk_len(), s.sign(&MSG));
        let sg = sig.len();
        let (wrong, flipped) = (s.sign(&OTHER), flip_mid(&sig));
        assert!(s.verify(&sig, &MSG) && !s.verify(&wrong, &MSG) && !s.verify(&flipped, &MSG));
        type Op<'a> = (&'a str, bool, Box<dyn FnMut() -> bool + 'a>);
        let s = std::cell::RefCell::new(s);
        let mut ops: Vec<Op> = vec![
            ("keygen", true, Box::new(|| s.borrow_mut().keygen())),
            (
                "sign",
                true,
                Box::new(|| !s.borrow_mut().sign(&MSG).is_empty()),
            ),
            (
                "verify_valid",
                true,
                Box::new(|| s.borrow_mut().verify(&sig, &MSG)),
            ),
            // well-formed signature over another message: full verification, then reject
            (
                "verify_invalid_msg",
                false,
                Box::new(|| s.borrow_mut().verify(&wrong, &MSG)),
            ),
            // one bit flipped in the middle of the wire bytes
            (
                "verify_invalid_flip",
                false,
                Box::new(|| s.borrow_mut().verify(&flipped, &MSG)),
            ),
        ];
        if s.borrow_mut().recover(&sig, &MSG).is_some() {
            ops.push((
                "recover",
                true,
                Box::new(|| s.borrow_mut().recover(&sig, &MSG) == Some(true)),
            ));
        }
        for (op, expect, f) in ops {
            eprintln!("{imp} {scheme} {op}");
            let mut v = measure(cfg, expect, f);
            if let Some(w) = csv.as_mut() {
                for x in &v {
                    writeln!(w, "{imp},{scheme},{op},{x}").unwrap();
                }
            }
            v.sort_unstable();
            let mean = v.iter().sum::<u64>() / v.len() as u64;
            let mut j = String::new();
            write!(
                j,
                "{{\"impl\":\"{imp}\",\"scheme\":\"{scheme}\",\"op\":\"{op}\",\"n\":{},\"min_ns\":{},\"mean_ns\":{mean},\
                 \"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"p99.9_ns\":{},\"max_ns\":{},\"pk_bytes\":{pk},\"sig_bytes\":{sg}}}",
                v.len(), v[0], pct(&v, 500), pct(&v, 950), pct(&v, 990), pct(&v, 999), v[v.len() - 1]
            )
            .unwrap();
            println!("{j}");
        }
    }
}

fn main() {
    let mut cfg = Cfg {
        min_n: 30,
        max_n: 1000,
        budget: Duration::from_secs(30),
        warmup: 5,
    };
    let (mut only, mut csv) = (String::new(), None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--version" => return println!("pq-lab {}", env!("CARGO_PKG_VERSION")),
            "--only" => only = val(),
            "--csv" => {
                csv = Some(std::io::BufWriter::new(
                    std::fs::File::create(val()).unwrap(),
                ))
            }
            "--min-n" => cfg.min_n = val().parse().unwrap(),
            "--max-n" => cfg.max_n = val().parse().unwrap(),
            "--budget-s" => cfg.budget = Duration::from_secs(val().parse().unwrap()),
            "--warmup" => cfg.warmup = val().parse().unwrap(),
            _ => panic!("unknown argument {a}; see the header of labs/pq/src/main.rs"),
        }
    }
    run(&cfg, &only, &mut csv);
    if let Some(w) = csv.as_mut() {
        w.flush().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_is_nearest_rank_like_summarise_py() {
        let v: Vec<u64> = (1..=1000).collect();
        assert_eq!((pct(&v, 500), pct(&v, 990), pct(&v, 999)), (500, 990, 999));
        // fewer than 1000 samples: p99.9 is the max, as bench/collect/summarise.py documents
        assert_eq!(pct(&v[..10], 999), 10);
        assert_eq!(pct(&v[..1], 500), 1);
    }

    /// Valid verifies, both invalid kinds reject, sizes are read from the keys. ML-DSA needs more
    /// stack than a test thread has in an unoptimised build. SLH-DSA is left to the real run:
    /// its sign is too slow for an unoptimised build.
    #[test]
    fn schemes_round_trip_and_reject() {
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                let cfg = Cfg {
                    min_n: 2,
                    max_n: 2,
                    budget: Duration::ZERO,
                    warmup: 0,
                };
                run(&cfg, "ECDSA", &mut None);
                run(&cfg, "ml-dsa/ML-DSA-44", &mut None);
                run(&cfg, "fips204/ML-DSA-44", &mut None);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

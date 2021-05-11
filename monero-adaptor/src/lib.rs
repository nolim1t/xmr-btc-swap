#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![warn(clippy::needless_pass_by_value)]

use anyhow::{bail, Result};
use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use hash_edwards_to_edwards::hash_point_to_point;
use rand::{CryptoRng, Rng};
use ring::Ring;
use std::convert::TryInto;
use tiny_keccak::{Hasher, Keccak};

mod ring;

pub const RING_SIZE: usize = 11;
const HASH_KEY_CLSAG_AGG_0: &str = "CLSAG_agg_0";
const HASH_KEY_CLSAG_AGG_1: &str = "CLSAG_agg_1";
const HASH_KEY_CLSAG_ROUND: &str = "CLSAG_round";

struct AggregationHashes {
    mu_P: Scalar,
    mu_C: Scalar,
}

impl AggregationHashes {
    pub fn new(
        ring: &Ring,
        commitment_ring: &Ring,
        I: EdwardsPoint,
        pseudo_output_commitment: EdwardsPoint,
        D: EdwardsPoint,
    ) -> Self {
        let I = I.compress();
        let D = D.compress();

        let pseudo_output_commitment = pseudo_output_commitment.compress();

        let mu_P = Self::hash(
            HASH_KEY_CLSAG_AGG_0,
            ring.as_ref(),
            commitment_ring.as_ref(),
            &I,
            &D,
            &pseudo_output_commitment,
        );
        let mu_C = Self::hash(
            HASH_KEY_CLSAG_AGG_1,
            ring.as_ref(),
            commitment_ring.as_ref(),
            &I,
            &D,
            &pseudo_output_commitment,
        );

        Self { mu_P, mu_C }
    }

    // aggregation hashes:
    // mu_{P, C} =
    // keccak256("CLSAG_agg_{0, 1}" ||
    //     ring || ring of commitments || I || z * hash_to_point(signing pk) ||
    // pseudooutput commitment)
    //
    // where z = blinding of real commitment - blinding of pseudooutput commitment.
    fn hash(
        domain_prefix: &str,
        ring: &[u8],
        commitment_ring: &[u8],
        I: &CompressedEdwardsY,
        z_key_image: &CompressedEdwardsY,
        pseudo_output_commitment: &CompressedEdwardsY,
    ) -> Scalar {
        let mut hasher = Keccak::v256();
        hasher.update(domain_prefix.as_bytes());
        hasher.update(ring);
        hasher.update(commitment_ring);
        hasher.update(I.as_bytes());
        hasher.update(z_key_image.as_bytes());
        hasher.update(pseudo_output_commitment.as_bytes());

        let mut hash = [0u8; 32];
        hasher.finalize(&mut hash);

        Scalar::from_bytes_mod_order(hash)
    }
}

// for every iteration we compute:
// c_p = h_prev * mu_P; and
// c_c = h_prev * mu_C.
//

// h = keccak256("CLSAG_round" || ring
//     ring of commitments || pseudooutput commitment || msg || L_i || R_i)

fn challenge(
    prefix: &[u8],
    s_i: Scalar,
    pk_i: EdwardsPoint,
    adjusted_commitment_i: EdwardsPoint,
    D: EdwardsPoint,
    h_prev: Scalar,
    I: EdwardsPoint,
    mus: &AggregationHashes,
) -> Result<Scalar> {
    let L_i = compute_L(h_prev, mus, s_i, pk_i, adjusted_commitment_i);
    let R_i = compute_R(h_prev, mus, pk_i, s_i, I, D);

    let mut hasher = Keccak::v256();
    hasher.update(prefix);
    hasher.update(&L_i.compress().as_bytes().to_vec());
    hasher.update(&R_i.compress().as_bytes().to_vec());

    let mut output = [0u8; 32];
    hasher.finalize(&mut output);

    Ok(Scalar::from_bytes_mod_order(output))
}

// L_i = s_i * G + c_p * pk_i + c_c * (commitment_i - pseudoutcommitment)
fn compute_L(
    h_prev: Scalar,
    mus: &AggregationHashes,
    s_i: Scalar,
    pk_i: EdwardsPoint,
    adjusted_commitment_i: EdwardsPoint,
) -> EdwardsPoint {
    let c_p = h_prev * mus.mu_P;
    let c_c = h_prev * mus.mu_C;

    (s_i * ED25519_BASEPOINT_POINT) + (c_p * pk_i) + c_c * adjusted_commitment_i
}

// R_i = s_i * H_p_pk_i + c_p * I + c_c * (z * hash_to_point(signing pk))
fn compute_R(
    h_prev: Scalar,
    mus: &AggregationHashes,
    pk_i: EdwardsPoint,
    s_i: Scalar,
    I: EdwardsPoint,
    D: EdwardsPoint,
) -> EdwardsPoint {
    let c_p = h_prev * mus.mu_P;
    let c_c = h_prev * mus.mu_C;

    let H_p_pk_i = hash_point_to_point(pk_i);

    (s_i * H_p_pk_i) + (c_p * I) + c_c * D
}

/// Compute the prefix for the hash common to every iteration of the ring
/// signature algorithm.
///
/// "CLSAG_round" || ring || ring of commitments || pseudooutput commitment ||
/// msg || alpha * G
fn clsag_round_hash_prefix(
    ring: &[u8],
    commitment_ring: &[u8],
    pseudo_output_commitment: EdwardsPoint,
    msg: &[u8],
) -> Vec<u8> {
    let domain_prefix = HASH_KEY_CLSAG_ROUND.as_bytes();
    let pseudo_output_commitment = pseudo_output_commitment.compress();
    let pseudo_output_commitment = pseudo_output_commitment.as_bytes();

    let mut prefix = Vec::with_capacity(
        domain_prefix.len()
            + ring.len()
            + commitment_ring.len()
            + pseudo_output_commitment.len()
            + msg.len(),
    );

    prefix.extend(domain_prefix);
    prefix.extend(ring);
    prefix.extend(commitment_ring);
    prefix.extend(pseudo_output_commitment);
    prefix.extend(msg);

    prefix
}

fn sign(
    fake_responses: [Scalar; RING_SIZE - 1],
    ring: Ring,
    commitment_ring: Ring,
    z: Scalar,
    H_p_pk: EdwardsPoint,
    pseudo_output_commitment: EdwardsPoint,
    L: EdwardsPoint,
    R: EdwardsPoint,
    I: EdwardsPoint,
    msg: &[u8],
    signing_key: Scalar,
    alpha: Scalar,
) -> Result<Signature> {
    let D = z * H_p_pk;
    let D_inv_8 = D * Scalar::from(8u8).invert();

    let prefix = clsag_round_hash_prefix(
        ring.as_ref(),
        commitment_ring.as_ref(),
        pseudo_output_commitment,
        msg,
    );
    let h_0 = {
        let mut keccak = Keccak::v256();
        keccak.update(&prefix);
        keccak.update(L.compress().as_bytes());
        keccak.update(R.compress().as_bytes());
        let mut output = [0u8; 32];
        keccak.finalize(&mut output);

        Scalar::from_bytes_mod_order(output)
    };

    let mus = AggregationHashes::new(&ring, &commitment_ring, I, pseudo_output_commitment, H_p_pk);

    let h_last = fake_responses
        .iter()
        .enumerate()
        .fold(h_0, |h_prev, (i, s_i)| {
            let pk_i = ring[i + 1];
            let adjusted_commitment_i = commitment_ring[i] - pseudo_output_commitment;

            // TODO: Do not unwrap here
            challenge(
                &prefix,
                *s_i,
                pk_i,
                adjusted_commitment_i,
                D_inv_8,
                h_prev,
                I,
                &mus,
            )
            .unwrap()
        });

    let s_last = alpha - h_last * ((mus.mu_P * signing_key) + (mus.mu_C * z));

    Ok(Signature {
        responses: [
            fake_responses[0],
            fake_responses[1],
            fake_responses[2],
            fake_responses[3],
            fake_responses[4],
            fake_responses[5],
            fake_responses[6],
            fake_responses[7],
            fake_responses[8],
            fake_responses[9],
            s_last,
        ],
        h_0,
        I,
        D,
    })
}

pub struct AdaptorSignature {
    s_0: Scalar,
    fake_responses: [Scalar; RING_SIZE - 1],
    h_0: Scalar,
    /// Key image of the real key in the ring.
    I: EdwardsPoint,
    /// Commitment key image `D = z * hash_to_p3(signing_public_key)`
    D: EdwardsPoint,
}

pub struct HalfAdaptorSignature {
    s_0_half: Scalar,
    fake_responses: [Scalar; RING_SIZE - 1],
    h_0: Scalar,
    /// Key image of the real key in the ring.
    I: EdwardsPoint,
    /// Commitment key image `D = z * hash_to_p3(signing_public_key)`
    D: EdwardsPoint,
}

impl HalfAdaptorSignature {
    fn complete(self, s_other_half: Scalar) -> AdaptorSignature {
        AdaptorSignature {
            s_0: self.s_0_half + s_other_half,
            fake_responses: self.fake_responses,
            h_0: self.h_0,
            I: self.I,
            D: self.D,
        }
    }
}

impl AdaptorSignature {
    pub fn adapt(self, y: Scalar) -> Signature {
        let r_last = self.s_0 + y;

        let responses = self
            .fake_responses
            .iter()
            .chain([r_last].iter())
            .copied()
            .collect::<Vec<_>>()
            .try_into()
            .expect("correct response size");

        Signature {
            responses,
            h_0: self.h_0,
            I: self.I,
            D: self.D,
        }
    }
}

pub struct Signature {
    pub responses: [Scalar; RING_SIZE],
    pub h_0: Scalar,
    /// Key image of the real key in the ring.
    pub I: EdwardsPoint,
    pub D: EdwardsPoint,
}

impl Signature {
    #[cfg(test)]
    fn verify(&self, ring: [EdwardsPoint; RING_SIZE], msg: &[u8; 32]) -> Result<bool> {
        let ring_concat = ring
            .iter()
            .flat_map(|pk| pk.compress().as_bytes().to_vec())
            .collect::<Vec<u8>>();

        let mut h = self.h_0;

        for (i, s_i) in self.responses.iter().enumerate() {
            let pk_i = ring[(i + 1) % RING_SIZE];
            h = challenge(
                &clsag_round_hash_prefix(&ring_concat, todo!(), todo!(), msg),
                *s_i,
                pk_i,
                todo!(),
                todo!(),
                h,
                self.I,
                todo!(),
            )?;
        }

        Ok(h == self.h_0)
    }
}

impl From<Signature> for monero::util::ringct::Clsag {
    fn from(from: Signature) -> Self {
        Self {
            s: from
                .responses
                .iter()
                .map(|s| monero::util::ringct::Key { key: s.to_bytes() })
                .collect(),
            c1: monero::util::ringct::Key {
                key: from.h_0.to_bytes(),
            },
            D: monero::util::ringct::Key {
                key: from.D.compress().to_bytes(),
            },
        }
    }
}

pub struct Alice0 {
    // secret index is always 0
    ring: Ring,
    fake_responses: [Scalar; RING_SIZE - 1],
    commitment_ring: Ring,
    pseudo_output_commitment: EdwardsPoint,
    msg: [u8; 32],
    // encryption key
    R_a: EdwardsPoint,
    // R'a = r_a*H_p(p_k) where p_k is the signing public key
    R_prime_a: EdwardsPoint,
    // this is not s_a cos of something to with one-time-address??
    s_prime_a: Scalar,
    // secret value:
    alpha_a: Scalar,
    H_p_pk: EdwardsPoint,
    I_a: EdwardsPoint,
    I_hat_a: EdwardsPoint,
    T_a: EdwardsPoint,
}

impl Alice0 {
    pub fn new(
        ring: [EdwardsPoint; RING_SIZE],
        msg: [u8; 32],
        commitment_ring: [EdwardsPoint; RING_SIZE],
        pseudo_output_commitment: EdwardsPoint,
        R_a: EdwardsPoint,
        R_prime_a: EdwardsPoint,
        s_prime_a: Scalar,
        rng: &mut (impl Rng + CryptoRng),
    ) -> Result<Self> {
        let ring = Ring::new(ring);
        let commitment_ring = Ring::new(commitment_ring);

        let mut fake_responses = [Scalar::zero(); RING_SIZE - 1];
        for response in fake_responses.iter_mut().take(RING_SIZE - 1) {
            *response = Scalar::random(rng);
        }
        let alpha_a = Scalar::random(rng);

        let p_k = ring[0];
        let H_p_pk = hash_point_to_point(p_k);

        let I_a = s_prime_a * H_p_pk;
        let I_hat_a = alpha_a * H_p_pk;
        let T_a = alpha_a * ED25519_BASEPOINT_POINT;

        Ok(Alice0 {
            ring,
            fake_responses,
            commitment_ring,
            pseudo_output_commitment,
            msg,
            R_a,
            R_prime_a,
            s_prime_a,
            alpha_a,
            H_p_pk,
            I_a,
            I_hat_a,
            T_a,
        })
    }

    pub fn next_message(&self, rng: &mut (impl Rng + CryptoRng)) -> Message0 {
        Message0 {
            pi_a: DleqProof::new(
                ED25519_BASEPOINT_POINT,
                self.T_a,
                self.H_p_pk,
                self.I_hat_a,
                self.alpha_a,
                rng,
            ),
            c_a: Commitment::new(self.fake_responses, self.I_a, self.I_hat_a, self.T_a),
        }
    }

    // TODO: Pass commitment-related data as an argument to this function, like z
    pub fn receive(self, msg: Message1, z: Scalar) -> Result<Alice1> {
        msg.pi_b
            .verify(ED25519_BASEPOINT_POINT, msg.T_b, self.H_p_pk, msg.I_hat_b)?;

        let sig = sign(
            self.fake_responses,
            self.ring,
            self.commitment_ring,
            z,
            self.H_p_pk,
            self.pseudo_output_commitment,
            self.T_a + msg.T_b + self.R_a,
            self.I_hat_a + msg.I_hat_b + self.R_prime_a,
            self.I_a + msg.I_b,
            &self.msg,
            self.s_prime_a,
            self.alpha_a,
        )?;

        let sig = HalfAdaptorSignature {
            s_0_half: sig.responses[10],
            fake_responses: self.fake_responses,
            h_0: sig.h_0,
            I: sig.I,
            D: sig.D,
        };

        Ok(Alice1 {
            fake_responses: self.fake_responses,
            I_a: self.I_a,
            I_hat_a: self.I_hat_a,
            T_a: self.T_a,
            sig,
        })
    }
}

pub struct Alice1 {
    fake_responses: [Scalar; RING_SIZE - 1],
    I_a: EdwardsPoint,
    I_hat_a: EdwardsPoint,
    T_a: EdwardsPoint,
    sig: HalfAdaptorSignature,
}

impl Alice1 {
    pub fn next_message(&self) -> Message2 {
        Message2 {
            d_a: Opening::new(self.fake_responses, self.I_a, self.I_hat_a, self.T_a),
            s_0_a: self.sig.s_0_half,
        }
    }

    pub fn receive(self, msg: Message3) -> Alice2 {
        let adaptor_sig = self.sig.complete(msg.s_0_b);

        Alice2 { adaptor_sig }
    }
}

pub struct Alice2 {
    pub adaptor_sig: AdaptorSignature,
}

pub struct Bob0 {
    ring: Ring,
    msg: [u8; 32],
    commitment_ring: Ring,
    pseudo_output_commitment: EdwardsPoint,
    R_a: EdwardsPoint,
    R_prime_a: EdwardsPoint,
    s_b: Scalar,
    alpha_b: Scalar,
    H_p_pk: EdwardsPoint,
    I_b: EdwardsPoint,
    I_hat_b: EdwardsPoint,
    T_b: EdwardsPoint,
}

impl Bob0 {
    pub fn new(
        ring: [EdwardsPoint; RING_SIZE],
        msg: [u8; 32],
        commitment_ring: [EdwardsPoint; RING_SIZE],
        pseudo_output_commitment: EdwardsPoint,
        R_a: EdwardsPoint,
        R_prime_a: EdwardsPoint,
        s_b: Scalar,
        rng: &mut (impl Rng + CryptoRng),
    ) -> Result<Self> {
        let ring = Ring::new(ring);
        let commitment_ring = Ring::new(commitment_ring);

        let alpha_b = Scalar::random(rng);

        let p_k = ring[0];
        let H_p_pk = hash_point_to_point(p_k);

        let I_b = s_b * H_p_pk;
        let I_hat_b = alpha_b * H_p_pk;
        let T_b = alpha_b * ED25519_BASEPOINT_POINT;

        Ok(Bob0 {
            ring,
            msg,
            commitment_ring,
            pseudo_output_commitment,
            R_a,
            R_prime_a,
            s_b,
            alpha_b,
            H_p_pk,
            I_b,
            I_hat_b,
            T_b,
        })
    }

    pub fn receive(self, msg: Message0) -> Bob1 {
        Bob1 {
            ring: self.ring,
            msg: self.msg,
            commitment_ring: self.commitment_ring,
            pseudo_output_commitment: self.pseudo_output_commitment,
            R_a: self.R_a,
            R_prime_a: self.R_prime_a,
            s_b: self.s_b,
            alpha_b: self.alpha_b,
            H_p_pk: self.H_p_pk,
            I_b: self.I_b,
            I_hat_b: self.I_hat_b,
            T_b: self.T_b,
            pi_a: msg.pi_a,
            c_a: msg.c_a,
        }
    }
}

pub struct Bob1 {
    ring: Ring,
    msg: [u8; 32],
    commitment_ring: Ring,
    pseudo_output_commitment: EdwardsPoint,
    R_a: EdwardsPoint,
    R_prime_a: EdwardsPoint,
    s_b: Scalar,
    alpha_b: Scalar,
    H_p_pk: EdwardsPoint,
    I_b: EdwardsPoint,
    I_hat_b: EdwardsPoint,
    T_b: EdwardsPoint,
    pi_a: DleqProof,
    c_a: Commitment,
}

impl Bob1 {
    pub fn next_message(&self, rng: &mut (impl Rng + CryptoRng)) -> Message1 {
        Message1 {
            I_b: self.I_b,
            T_b: self.T_b,
            I_hat_b: self.I_hat_b,
            pi_b: DleqProof::new(
                ED25519_BASEPOINT_POINT,
                self.T_b,
                self.H_p_pk,
                self.I_hat_b,
                self.alpha_b,
                rng,
            ),
        }
    }

    // TODO: Pass commitment-related data as an argument to this function, like z
    pub fn receive(self, msg: Message2, z: Scalar) -> Result<Bob2> {
        let (fake_responses, I_a, I_hat_a, T_a) = msg.d_a.open(self.c_a)?;

        self.pi_a
            .verify(ED25519_BASEPOINT_POINT, T_a, self.H_p_pk, I_hat_a)?;

        let I = I_a + self.I_b;
        let sig = sign(
            fake_responses,
            self.ring,
            self.commitment_ring,
            z,
            self.H_p_pk,
            self.pseudo_output_commitment,
            T_a + self.T_b + self.R_a,
            I_hat_a + self.I_hat_b + self.R_prime_a,
            I,
            &self.msg,
            self.s_b,
            self.alpha_b,
        )?;

        let s_0_b = sig.responses[10];
        let sig = HalfAdaptorSignature {
            s_0_half: s_0_b,
            fake_responses,
            h_0: sig.h_0,
            I: sig.I,
            D: sig.D,
        };
        let adaptor_sig = sig.complete(msg.s_0_a);

        Ok(Bob2 { s_0_b, adaptor_sig })
    }
}

pub struct Bob2 {
    s_0_b: Scalar,
    pub adaptor_sig: AdaptorSignature,
}

impl Bob2 {
    pub fn next_message(&self) -> Message3 {
        Message3 { s_0_b: self.s_0_b }
    }
}

struct DleqProof {
    s: Scalar,
    c: Scalar,
}

impl DleqProof {
    fn new(
        G: EdwardsPoint,
        xG: EdwardsPoint,
        H: EdwardsPoint,
        xH: EdwardsPoint,
        x: Scalar,
        rng: &mut (impl Rng + CryptoRng),
    ) -> Self {
        let r = Scalar::random(rng);
        let rG = r * G;
        let rH = r * H;

        let mut keccak = Keccak::v256();
        keccak.update(G.compress().as_bytes());
        keccak.update(xG.compress().as_bytes());
        keccak.update(H.compress().as_bytes());
        keccak.update(xH.compress().as_bytes());
        keccak.update(rG.compress().as_bytes());
        keccak.update(rH.compress().as_bytes());

        let mut output = [0u8; 32];
        keccak.finalize(&mut output);

        let c = Scalar::from_bytes_mod_order(output);

        let s = r + c * x;

        Self { s, c }
    }

    fn verify(
        &self,
        G: EdwardsPoint,
        xG: EdwardsPoint,
        H: EdwardsPoint,
        xH: EdwardsPoint,
    ) -> Result<()> {
        let s = self.s;
        let c = self.c;

        let rG = (s * G) + (-c * xG);
        let rH = (s * H) + (-c * xH);

        let mut keccak = Keccak::v256();
        keccak.update(G.compress().as_bytes());
        keccak.update(xG.compress().as_bytes());
        keccak.update(H.compress().as_bytes());
        keccak.update(xH.compress().as_bytes());
        keccak.update(rG.compress().as_bytes());
        keccak.update(rH.compress().as_bytes());

        let mut output = [0u8; 32];
        keccak.finalize(&mut output);

        let c_prime = Scalar::from_bytes_mod_order(output);

        if c != c_prime {
            bail!("invalid DLEQ proof")
        }

        Ok(())
    }
}

#[derive(PartialEq)]
struct Commitment([u8; 32]);

impl Commitment {
    fn new(
        fake_responses: [Scalar; RING_SIZE - 1],
        I_a: EdwardsPoint,
        I_hat_a: EdwardsPoint,
        T_a: EdwardsPoint,
    ) -> Self {
        let fake_responses = fake_responses
            .iter()
            .flat_map(|r| r.as_bytes().to_vec())
            .collect::<Vec<u8>>();

        let mut keccak = Keccak::v256();
        keccak.update(&fake_responses);
        keccak.update(I_a.compress().as_bytes());
        keccak.update(I_hat_a.compress().as_bytes());
        keccak.update(T_a.compress().as_bytes());

        let mut output = [0u8; 32];
        keccak.finalize(&mut output);

        Self(output)
    }
}

struct Opening {
    fake_responses: [Scalar; RING_SIZE - 1],
    I_a: EdwardsPoint,
    I_hat_a: EdwardsPoint,
    T_a: EdwardsPoint,
}

impl Opening {
    fn new(
        fake_responses: [Scalar; RING_SIZE - 1],
        I_a: EdwardsPoint,
        I_hat_a: EdwardsPoint,
        T_a: EdwardsPoint,
    ) -> Self {
        Self {
            fake_responses,
            I_a,
            I_hat_a,
            T_a,
        }
    }

    fn open(
        self,
        commitment: Commitment,
    ) -> Result<(
        [Scalar; RING_SIZE - 1],
        EdwardsPoint,
        EdwardsPoint,
        EdwardsPoint,
    )> {
        let self_commitment =
            Commitment::new(self.fake_responses, self.I_a, self.I_hat_a, self.T_a);

        if self_commitment == commitment {
            Ok((self.fake_responses, self.I_a, self.I_hat_a, self.T_a))
        } else {
            bail!("opening does not match commitment")
        }
    }
}

// Alice Sends this to Bob
pub struct Message0 {
    c_a: Commitment,
    pi_a: DleqProof,
}

// Bob sends this to ALice
pub struct Message1 {
    I_b: EdwardsPoint,
    T_b: EdwardsPoint,
    I_hat_b: EdwardsPoint,
    pi_b: DleqProof,
}

// Alice sends this to Bob
pub struct Message2 {
    d_a: Opening,
    s_0_a: Scalar,
}

// Bob sends this to Alice
#[derive(Clone, Copy)]
pub struct Message3 {
    s_0_b: Scalar,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn sign_and_verify_success() {
        let msg_to_sign = b"hello world, monero is amazing!!";

        let s_prime_a = Scalar::random(&mut OsRng);
        let s_b = Scalar::random(&mut OsRng);

        let pk = (s_prime_a + s_b) * ED25519_BASEPOINT_POINT;

        let (r_a, R_a, R_prime_a) = {
            let r_a = Scalar::random(&mut OsRng);
            let R_a = r_a * ED25519_BASEPOINT_POINT;

            let pk_hashed_to_point = hash_point_to_point(pk);

            let R_prime_a = r_a * pk_hashed_to_point;

            (r_a, R_a, R_prime_a)
        };

        let mut ring = [EdwardsPoint::default(); RING_SIZE];
        ring[0] = pk;

        ring[1..].fill_with(|| {
            let x = Scalar::random(&mut OsRng);
            x * ED25519_BASEPOINT_POINT
        });

        let mut commitment_ring = [EdwardsPoint::default(); RING_SIZE];

        let real_commitment_blinding = Scalar::random(&mut OsRng);
        commitment_ring[0] = real_commitment_blinding * ED25519_BASEPOINT_POINT; // + 0 * H
        commitment_ring[1..].fill_with(|| {
            let x = Scalar::random(&mut OsRng);
            x * ED25519_BASEPOINT_POINT
        });

        // TODO: document
        let pseudo_output_commitment = commitment_ring[0];

        let alice = Alice0::new(
            ring,
            *msg_to_sign,
            commitment_ring,
            pseudo_output_commitment,
            R_a,
            R_prime_a,
            s_prime_a,
            &mut OsRng,
        )
        .unwrap();
        let bob = Bob0::new(
            ring,
            *msg_to_sign,
            commitment_ring,
            pseudo_output_commitment,
            R_a,
            R_prime_a,
            s_b,
            &mut OsRng,
        )
        .unwrap();

        let msg = alice.next_message(&mut OsRng);
        let bob = bob.receive(msg);

        // TODO: Document this
        let msg = bob.next_message(&mut OsRng);
        let alice = alice.receive(msg, Scalar::zero()).unwrap();

        let msg = alice.next_message();
        let bob = bob.receive(msg, Scalar::zero()).unwrap();

        let msg = bob.next_message();
        let alice = alice.receive(msg);

        let sig = alice.adaptor_sig.adapt(r_a);

        assert!(sig.verify(ring, msg_to_sign).unwrap());
    }
}
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use hash_edwards_to_edwards::hash_point_to_point;
use tiny_keccak::{Hasher, Keccak};

use crate::ring::Ring;
use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;

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

fn challenge(
    prefix: &[u8],
    s_i: Scalar,
    pk_i: EdwardsPoint,
    adjusted_commitment_i: EdwardsPoint,
    D: EdwardsPoint,
    h_prev: Scalar,
    I: EdwardsPoint,
    mus: &AggregationHashes,
) -> anyhow::Result<Scalar> {
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

pub fn sign(
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
) -> anyhow::Result<Signature> {
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

pub struct Signature {
    pub responses: [Scalar; RING_SIZE],
    pub h_0: Scalar,
    /// Key image of the real key in the ring.
    pub I: EdwardsPoint,
    pub D: EdwardsPoint,
}

impl Signature {
    #[cfg(test)]
    pub fn verify(&self, ring: [EdwardsPoint; RING_SIZE], msg: &[u8; 32]) -> anyhow::Result<bool> {
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

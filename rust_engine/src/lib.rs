use std::arch::x86_64::*;

#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
pub struct Record {
    pub vector: [f32; 16],
}

#[repr(C, align(64))]
struct Aligned<T: ?Sized>(T);

static DATASET_BYTES: &Aligned<[u8]> = &Aligned(*include_bytes!("../dataset.bin"));
static L1_BYTES: &Aligned<[u8]> = &Aligned(*include_bytes!("../l1_centroids.bin"));
static L2_BYTES: &Aligned<[u8]> = &Aligned(*include_bytes!("../l2_centroids.bin"));
static OFFSETS_BYTES: &Aligned<[u8]> = &Aligned(*include_bytes!("../offsets.bin"));

static mut RECORDS: Option<&'static [Record]> = None;
static mut L1_CENTROIDS: Option<&'static [[f32; 16]]> = None;
static mut L2_CENTROIDS: Option<&'static [[f32; 16]]> = None;
static mut OFFSETS: Option<&'static [u32]> = None;

#[no_mangle]
pub extern "C" fn init_engine() -> i32 {
    unsafe {
        let records_len = DATASET_BYTES.0.len() / 64;
        let records_ptr = DATASET_BYTES.0.as_ptr() as *const Record;
        RECORDS = Some(std::slice::from_raw_parts(records_ptr, records_len));

        let l1_len = L1_BYTES.0.len() / 64;
        let l1_ptr = L1_BYTES.0.as_ptr() as *const [f32; 16];
        L1_CENTROIDS = Some(std::slice::from_raw_parts(l1_ptr, l1_len));

        let l2_len = L2_BYTES.0.len() / 64;
        let l2_ptr = L2_BYTES.0.as_ptr() as *const [f32; 16];
        L2_CENTROIDS = Some(std::slice::from_raw_parts(l2_ptr, l2_len));

        let offsets_len = OFFSETS_BYTES.0.len() / 4;
        let offsets_ptr = OFFSETS_BYTES.0.as_ptr() as *const u32;
        OFFSETS = Some(std::slice::from_raw_parts(offsets_ptr, offsets_len));
    }
    0
}

const K: usize = 7;
const N_L1: usize = 256;
const N_L2_PER_L1: usize = 256;
const N_PROBE_L1: usize = 16;
const N_PROBE_L2: usize = 256;
const N_PROBE_L2_EXTENDED: usize = 512;
const CONFIDENCE_THRESHOLD_LOW: f32 = 0.38;
const CONFIDENCE_THRESHOLD_HIGH: f32 = 0.50;

#[inline(always)]
#[cfg(target_arch = "x86_64")]

unsafe fn dist_avx2_x4(q: &[f32; 16], v0: &[f32; 16], v1: &[f32; 16], v2: &[f32; 16], v3: &[f32; 16]) -> (f32, f32, f32, f32) {
    let q_low = _mm256_loadu_ps(q.as_ptr());
    let q_high = _mm256_loadu_ps(q.as_ptr().add(8));
    let abs_mask = _mm256_castsi256_ps(_mm256_set1_epi32(0x7fffffff));

    let b0_low = _mm256_loadu_ps(v0.as_ptr());
    let b1_low = _mm256_loadu_ps(v1.as_ptr());
    let b2_low = _mm256_loadu_ps(v2.as_ptr());
    let b3_low = _mm256_loadu_ps(v3.as_ptr());
    let b0_high = _mm256_loadu_ps(v0.as_ptr().add(8));
    let b1_high = _mm256_loadu_ps(v1.as_ptr().add(8));
    let b2_high = _mm256_loadu_ps(v2.as_ptr().add(8));
    let b3_high = _mm256_loadu_ps(v3.as_ptr().add(8));
    
    let s0 = _mm256_add_ps(_mm256_and_ps(_mm256_sub_ps(q_low, b0_low), abs_mask), _mm256_and_ps(_mm256_sub_ps(q_high, b0_high), abs_mask));
    let s1 = _mm256_add_ps(_mm256_and_ps(_mm256_sub_ps(q_low, b1_low), abs_mask), _mm256_and_ps(_mm256_sub_ps(q_high, b1_high), abs_mask));
    let s2 = _mm256_add_ps(_mm256_and_ps(_mm256_sub_ps(q_low, b2_low), abs_mask), _mm256_and_ps(_mm256_sub_ps(q_high, b2_high), abs_mask));
    let s3 = _mm256_add_ps(_mm256_and_ps(_mm256_sub_ps(q_low, b3_low), abs_mask), _mm256_and_ps(_mm256_sub_ps(q_high, b3_high), abs_mask));

    let sum01 = _mm256_hadd_ps(s0, s1);
    let sum23 = _mm256_hadd_ps(s2, s3);
    let sum0123 = _mm256_hadd_ps(sum01, sum23);
    let low128 = _mm256_castps256_ps128(sum0123);
    let high128 = _mm256_extractf128_ps(sum0123, 1);
    let final_sums = _mm_add_ps(low128, high128);
    
    let arr: [f32; 4] = std::mem::transmute(final_sums);
    (arr[0], arr[1], arr[2], arr[3])
}

#[inline(always)]
#[cfg(target_arch = "x86_64")]

unsafe fn dist_avx2(q: &[f32; 16], v2: &[f32; 16]) -> f32 {
    let q_low = _mm256_loadu_ps(q.as_ptr());
    let q_high = _mm256_loadu_ps(q.as_ptr().add(8));
    let abs_mask = _mm256_castsi256_ps(_mm256_set1_epi32(0x7fffffff));

    let b_low = _mm256_loadu_ps(v2.as_ptr());
    let b_high = _mm256_loadu_ps(v2.as_ptr().add(8));
    let diff_low = _mm256_and_ps(_mm256_sub_ps(q_low, b_low), abs_mask);
    let diff_high = _mm256_and_ps(_mm256_sub_ps(q_high, b_high), abs_mask);
    let sum_vec = _mm256_add_ps(diff_low, diff_high);
    
    let x128_low = _mm256_castps256_ps128(sum_vec);
    let x128_high = _mm256_extractf128_ps(sum_vec, 1);
    let x_sum = _mm_add_ps(x128_low, x128_high);
    let x_shuf = _mm_movehdup_ps(x_sum);
    let x_sum2 = _mm_add_ps(x_sum, x_shuf);
    let x_shuf2 = _mm_movehl_ps(x_sum2, x_sum2);
    let x_final = _mm_add_ss(x_sum2, x_shuf2);
    _mm_cvtss_f32(x_final)
}

#[inline(always)]
fn update_top_k_packed(top_k: &mut [(f32, u8, usize); K], dist: f32, packed_f32: f32) -> f32 {
    let packed = packed_f32.to_bits();
    let label = (packed & 1) as u8;
    let id = (packed >> 1) as usize;

    if dist < top_k[K - 1].0 {
        let mut pos = K - 1;
        while pos > 0 && dist < top_k[pos - 1].0 {
            top_k[pos] = top_k[pos - 1];
            pos -= 1;
        }
        top_k[pos] = (dist, label, id);
    }
    top_k[K - 1].0
}

#[inline(always)]
#[cfg(target_arch = "x86_64")]

unsafe fn scan_cluster(
    l2_idx: usize,
    records: &[Record],
    offsets: &[u32],
    q: &[f32; 16],
    top_k: &mut [(f32, u8, usize); K],
    mut max_dist: f32
) -> f32 {
    let start = offsets[l2_idx] as usize;
    let end = offsets[l2_idx + 1] as usize;
    let cluster_records = &records[start..end];

    let mut k = 0;
    while k + 7 < cluster_records.len() {
        if k + 24 < cluster_records.len() {
            _mm_prefetch(cluster_records.as_ptr().add(k + 24) as *const i8, _MM_HINT_T0);
        }
        let (d0, d1, d2, d3) = dist_avx2_x4(
            q,
            &cluster_records.get_unchecked(k).vector,
            &cluster_records.get_unchecked(k+1).vector,
            &cluster_records.get_unchecked(k+2).vector,
            &cluster_records.get_unchecked(k+3).vector
        );
        if d0 < max_dist { max_dist = update_top_k_packed(top_k, d0, cluster_records.get_unchecked(k).vector[15]); }
        if d1 < max_dist { max_dist = update_top_k_packed(top_k, d1, cluster_records.get_unchecked(k+1).vector[15]); }
        if d2 < max_dist { max_dist = update_top_k_packed(top_k, d2, cluster_records.get_unchecked(k+2).vector[15]); }
        if d3 < max_dist { max_dist = update_top_k_packed(top_k, d3, cluster_records.get_unchecked(k+3).vector[15]); }

        let (d4, d5, d6, d7) = dist_avx2_x4(
            q,
            &cluster_records.get_unchecked(k+4).vector,
            &cluster_records.get_unchecked(k+5).vector,
            &cluster_records.get_unchecked(k+6).vector,
            &cluster_records.get_unchecked(k+7).vector
        );
        if d4 < max_dist { max_dist = update_top_k_packed(top_k, d4, cluster_records.get_unchecked(k+4).vector[15]); }
        if d5 < max_dist { max_dist = update_top_k_packed(top_k, d5, cluster_records.get_unchecked(k+5).vector[15]); }
        if d6 < max_dist { max_dist = update_top_k_packed(top_k, d6, cluster_records.get_unchecked(k+6).vector[15]); }
        if d7 < max_dist { max_dist = update_top_k_packed(top_k, d7, cluster_records.get_unchecked(k+7).vector[15]); }
        
        k += 8;
    }
    while k < cluster_records.len() {
        let dist = dist_avx2(q, &cluster_records[k].vector);
        if dist < max_dist {
            max_dist = update_top_k_packed(top_k, dist, cluster_records[k].vector[15]);
        }
        k += 1;
    }
    max_dist
}

fn calculate_fraud_score_weighted(top_k: [(f32, u8, usize); K]) -> f32 {
    if top_k[0].0 < 0.05 {
        let label = top_k[0].1;
        return label as f32;
    }
    let mut fraud_weight = 0.0f32;
    let mut total_weight = 0.0f32;
    for &(dist, label, _) in &top_k {
        if dist == f32::MAX { continue; }
        let weight = (-dist * 0.5).exp();
        total_weight += weight;
        fraud_weight += weight * label as f32;
    }
    if total_weight > 0.0 { fraud_weight / total_weight } else { 0.0 }
}

#[cfg(target_arch = "x86_64")]

unsafe fn search_hivf_avx2(query: &[f32; 16]) -> f32 {
    let l1_centroids = L1_CENTROIDS.unwrap();
    let l2_centroids = L2_CENTROIDS.unwrap();
    let offsets = OFFSETS.unwrap();
    let records = RECORDS.unwrap();

    // 1. Scan L1 Root
    thread_local! {
        static DISTS_L1: std::cell::RefCell<Vec<(f32, usize)>> = std::cell::RefCell::new(vec![(0.0, 0); N_L1]);
        static DISTS_L2: std::cell::RefCell<Vec<(f32, usize)>> = std::cell::RefCell::new(vec![(0.0, 0); N_PROBE_L1 * N_L2_PER_L1]);
    }

    let mut best_l1_copy = [(0.0f32, 0usize); N_PROBE_L1];
    let mut best_l2_unsorted_copy = [(0.0f32, 0usize); N_PROBE_L2_EXTENDED];

    DISTS_L1.with(|d1| {
        let mut dists_l1 = d1.borrow_mut();
        let mut i = 0;
        while i + 3 < N_L1 {
            let (d0, d1_dist, d2, d3) = dist_avx2_x4(
                query,
                &l1_centroids[i], &l1_centroids[i+1], &l1_centroids[i+2], &l1_centroids[i+3]
            );
            dists_l1[i] = (d0, i);
            dists_l1[i+1] = (d1_dist, i+1);
            dists_l1[i+2] = (d2, i+2);
            dists_l1[i+3] = (d3, i+3);
            i += 4;
        }

        let (best_l1, _, _) = dists_l1.select_nth_unstable_by(N_PROBE_L1, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        best_l1_copy.copy_from_slice(&best_l1[0..N_PROBE_L1]);
    });

    // 2. Scan L2 Leaves for selected L1s
    DISTS_L2.with(|d2| {
        let mut dists_l2 = d2.borrow_mut();
        let mut l2_count = 0;
        for probe_idx in 0..N_PROBE_L1 {
            let l1_idx = best_l1_copy[probe_idx].1;
            let l2_base = l1_idx * N_L2_PER_L1;
            let mut j = 0;
            while j + 7 < N_L2_PER_L1 {
                let (d0, d1, d2, d3) = dist_avx2_x4(
                    query,
                    &l2_centroids[l2_base + j], &l2_centroids[l2_base + j + 1], 
                    &l2_centroids[l2_base + j + 2], &l2_centroids[l2_base + j + 3]
                );
                let (d4, d5, d6, d7) = dist_avx2_x4(
                    query,
                    &l2_centroids[l2_base + j + 4], &l2_centroids[l2_base + j + 5], 
                    &l2_centroids[l2_base + j + 6], &l2_centroids[l2_base + j + 7]
                );

                dists_l2[l2_count] = (d0, l2_base + j);
                dists_l2[l2_count+1] = (d1, l2_base + j + 1);
                dists_l2[l2_count+2] = (d2, l2_base + j + 2);
                dists_l2[l2_count+3] = (d3, l2_base + j + 3);
                dists_l2[l2_count+4] = (d4, l2_base + j + 4);
                dists_l2[l2_count+5] = (d5, l2_base + j + 5);
                dists_l2[l2_count+6] = (d6, l2_base + j + 6);
                dists_l2[l2_count+7] = (d7, l2_base + j + 7);
                
                j += 8;
                l2_count += 8;
            }
        }

        let (best_l2_unsorted, _, _) = dists_l2.select_nth_unstable_by(N_PROBE_L2_EXTENDED, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let (best_l2_first_256, _, _) = best_l2_unsorted.select_nth_unstable_by(N_PROBE_L2, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        best_l2_first_256.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        best_l2_unsorted_copy.copy_from_slice(&best_l2_unsorted[0..N_PROBE_L2_EXTENDED]);
    });

    let best_l2 = &best_l2_unsorted_copy;

    // 3. Scan Records in top 256 clusters
    let mut top_k = [(f32::MAX, 0u8, 0usize); K];
    let mut max_dist = f32::MAX;

    for probe_idx in 0..N_PROBE_L2 {
        max_dist = scan_cluster(best_l2[probe_idx].1, records, offsets, query, &mut top_k, max_dist);
        if top_k[0].0 < 0.05 {
            let label = top_k[0].1;
            return label as f32;
        }
    }

    let score = calculate_fraud_score_weighted(top_k);

    // 4. Adaptive Probing
    if (score > CONFIDENCE_THRESHOLD_LOW && score < CONFIDENCE_THRESHOLD_HIGH) || top_k[0].0 > 1.5 {
        let mut extended_top_k = top_k;
        let mut extended_max_dist = max_dist;
        for probe_idx in N_PROBE_L2..N_PROBE_L2_EXTENDED {
            extended_max_dist = scan_cluster(best_l2[probe_idx].1, records, offsets, query, &mut extended_top_k, extended_max_dist);
            if extended_top_k[0].0 < 0.05 {
                let label = extended_top_k[0].1;
                return label as f32;
            }
        }
        return calculate_fraud_score_weighted(extended_top_k);
    }

    score
}

#[no_mangle]
pub unsafe extern "C" fn search_vector(query_ptr: *const f32, _force_deep: i32) -> f32 {
    let q_in = std::slice::from_raw_parts(query_ptr, 16);
    let mut query = [0.0f32; 16];
    query.copy_from_slice(q_in);
    
    // Fallback to scalar disabled since AVX2 is guaranteed on target
    search_hivf_avx2(&query)
}

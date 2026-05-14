use std::ffi::CStr;
use std::fs::File;
use std::os::raw::c_char;
use std::slice;
use memmap2::{Mmap, MmapOptions};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

static mut DATASET_MMAP: Option<Mmap> = None;
static mut CENTROIDS: Option<&'static [[f32; 16]]> = None;
static mut BBOXES: Option<&'static [[i16; 32]]> = None;
static mut OFFSETS: Option<&'static [u32]> = None;
static mut SIZES: Option<&'static [u32]> = None;
static mut NUM_CLUSTERS: usize = 0;

#[repr(C, align(32))]
struct AlignedQuery {
    q: [f32; 16],
}

#[no_mangle]
pub extern "C" fn init_engine(path_ptr: *const c_char) -> i32 {
    unsafe {
        let c_str = CStr::from_ptr(path_ptr);
        let path = match c_str.to_str() { Ok(s) => s, Err(_) => return -1 };
        let file = match File::open(path) { Ok(f) => f, Err(_) => return -2 };
        let mmap = match MmapOptions::new().map(&file) { Ok(m) => m, Err(_) => return -3 };

        libc::mlock(mmap.as_ptr() as *const libc::c_void, mmap.len());

        let header = slice::from_raw_parts(mmap.as_ptr() as *const u32, 16);
        if header[0] != 0x4E495452 { return -4; }

        let k = header[2] as usize;
        NUM_CLUSTERS = k;

        let centroids_ptr = mmap.as_ptr().add(64) as *const [f32; 16];
        CENTROIDS = Some(slice::from_raw_parts(centroids_ptr, k));

        let bboxes_ptr = mmap.as_ptr().add(64 + k * 64) as *const [i16; 32];
        BBOXES = Some(slice::from_raw_parts(bboxes_ptr, k));

        let offsets_ptr = mmap.as_ptr().add(64 + k * 64 + k * 64) as *const u32;
        OFFSETS = Some(slice::from_raw_parts(offsets_ptr, k));

        let sizes_ptr = mmap.as_ptr().add(64 + k * 64 + k * 64 + k * 4) as *const u32;
        SIZES = Some(slice::from_raw_parts(sizes_ptr, k));

        DATASET_MMAP = Some(mmap);
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn search_vector(query_ptr: *const f32, force_deep: i32) -> i32 {
    let q_in = slice::from_raw_parts(query_ptr, 14);
    let mut aq = AlignedQuery { q: [0.0f32; 16] };
    aq.q[0..14].copy_from_slice(q_in);
    let q = aq.q;

    let centroids = match CENTROIDS { Some(c) => c, None => return 0 };
    let bboxes = match BBOXES { Some(b) => b, None => return 0 };
    let mmap_ref = match DATASET_MMAP.as_ref() { Some(m) => m, None => return 0 };
    let mmap_ptr = mmap_ref.as_ptr();
    let offsets = OFFSETS.unwrap();
    let sizes = SIZES.unwrap();
    let num_k = NUM_CLUSTERS;

    let mut top_dists = [f32::MAX; 5];
    let mut top_labels = [0u32; 5];
    let mut top_indices = [u32::MAX; 5];

    let mut centroid_dists = [(0.0f32, 0usize); 4096];
    let n_centroids = std::cmp::min(num_k, 4096);

    let q_simd: [__m256; 2] = [
        _mm256_loadu_ps(q.as_ptr()),
        _mm256_loadu_ps(q.as_ptr().add(8))
    ];

    for ki in (0..n_centroids).step_by(1) {
        let d2 = dist_sq_f32_arch(q.as_ptr(), centroids[ki].as_ptr());
        centroid_dists[ki] = (d2, ki);
    }

    let sub = &mut centroid_dists[0..n_centroids];
    sub.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let nprobe = if force_deep == 1 { 192 } else { 8 };

    for i in 0..nprobe {
        if i >= sub.len() { break; }
        let ki = sub[i].1;
        if min_dist_to_bbox(&q, &bboxes[ki]) > top_dists[4] { continue; }
        scan_cluster_soa(ki, &q, mmap_ptr, offsets, sizes, &mut top_dists, &mut top_indices, &mut top_labels);
    }

    top_labels.iter().sum::<u32>() as i32
}

#[inline(always)]
unsafe fn min_dist_to_bbox(q: &[f32; 16], bbox: &[i16; 32]) -> f32 {
    let mut dist2 = 0.0f32;
    const SCALE: f32 = 0.0001;
    for d in 0..14 {
        let b_min = bbox[d] as f32 * SCALE;
        let b_max = bbox[d+16] as f32 * SCALE;
        let qd = q[d];
        if qd < b_min {
            let diff = b_min - qd;
            dist2 += diff * diff;
        } else if qd > b_max {
            let diff = qd - b_max;
            dist2 += diff * diff;
        }
    }
    dist2
}

#[inline(always)]
unsafe fn scan_cluster_soa(ki: usize, q: &[f32; 16], mmap_ptr: *const u8, offsets: &[u32], sizes: &[u32], top_dists: &mut [f32; 5], top_indices: &mut [u32; 5], top_labels: &mut [u32; 5]) {
    let n = sizes[ki] as usize;
    if n == 0 { return; }
    let num_blocks = (n + 7) / 8;
    let base_ptr = mmap_ptr.add(offsets[ki] as usize);

    let q_simd: [__m256; 14] = [
        _mm256_set1_ps(q[0]), _mm256_set1_ps(q[1]), _mm256_set1_ps(q[2]), _mm256_set1_ps(q[3]),
        _mm256_set1_ps(q[4]), _mm256_set1_ps(q[5]), _mm256_set1_ps(q[6]), _mm256_set1_ps(q[7]),
        _mm256_set1_ps(q[8]), _mm256_set1_ps(q[9]), _mm256_set1_ps(q[10]), _mm256_set1_ps(q[11]),
        _mm256_set1_ps(q[12]), _mm256_set1_ps(q[13])
    ];
    let scale = _mm256_set1_ps(0.0001);

    for bi in 0..num_blocks {
        let block_ptr = base_ptr.add(bi * 256);
        #[cfg(target_arch = "x86_64")]
        _mm_prefetch(base_ptr.add((bi + 2) * 256) as *const i8, _MM_HINT_T0);

        let mut acc = _mm256_setzero_ps();
        for d in 0..8 {
            let v_i16 = _mm_loadu_si128(block_ptr.add(d * 16) as *const __m128i);
            let v_f32 = _mm256_mul_ps(_mm256_cvtepi32_ps(_mm256_cvtepi16_epi32(v_i16)), scale);
            let diff = _mm256_sub_ps(v_f32, q_simd[d]);
            acc = _mm256_fmadd_ps(diff, diff, acc);
        }

        let threshold = _mm256_set1_ps(top_dists[4]);
        if _mm256_movemask_ps(_mm256_cmp_ps(acc, threshold, _CMP_GT_OQ)) == 0xFF {
            continue;
        }

        for d in 8..14 {
            let v_i16 = _mm_loadu_si128(block_ptr.add(d * 16) as *const __m128i);
            let v_f32 = _mm256_mul_ps(_mm256_cvtepi32_ps(_mm256_cvtepi16_epi32(v_i16)), scale);
            let diff = _mm256_sub_ps(v_f32, q_simd[d]);
            acc = _mm256_fmadd_ps(diff, diff, acc);
        }

        let dists: [f32; 8] = std::mem::transmute(acc);
        let metas = slice::from_raw_parts(block_ptr.add(224) as *const u32, 8);

        for i in 0..8 {
            let d2 = dists[i];
            if d2 <= top_dists[4] {
                let meta = metas[i];
                let index = meta & 0x7FFFFFFF;
                if index != 0x7FFFFFFF {
                    insert_top5(d2, index, (meta >> 31) & 1, top_dists, top_indices, top_labels);
                }
            }
        }
    }
}

#[inline(always)]
unsafe fn dist_sq_f32_arch(q: *const f32, p: *const f32) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        let q_v = _mm256_loadu_ps(q);
        let p_v = _mm256_loadu_ps(p);
        let diff = _mm256_sub_ps(q_v, p_v);
        let mut sq = _mm256_mul_ps(diff, diff);
        let q_v2 = _mm256_loadu_ps(q.add(8));
        let p_v2 = _mm256_loadu_ps(p.add(8));
        let diff2 = _mm256_sub_ps(q_v2, p_v2);
        sq = _mm256_fmadd_ps(diff2, diff2, sq);
        hsum_ps_avx(sq)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let mut d2 = 0.0f32;
        for d in 0..14 {
            let diff = *q.add(d) - *p.add(d);
            d2 += diff * diff;
        }
        d2
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn hsum_ps_avx(v: __m256) -> f32 {
    let x128 = _mm_add_ps(_mm256_extractf128_ps(v, 1), _mm256_castps256_ps128(v));
    let x64 = _mm_add_ps(x128, _mm_movehl_ps(x128, x128));
    let x32 = _mm_add_ss(x64, _mm_shuffle_ps(x64, x64, 0x55));
    _mm_cvtss_f32(x32)
}

#[inline(always)]
fn insert_top5(dist: f32, index: u32, label: u32, dists: &mut [f32; 5], indices: &mut [u32; 5], labels: &mut [u32; 5]) {
    let mut pos = 0;
    while pos < 5 {
        if dist < dists[pos] || (dist == dists[pos] && index < indices[pos]) { break; }
        pos += 1;
    }
    if pos < 5 {
        for i in (pos+1..5).rev() {
            dists[i] = dists[i-1]; indices[i] = indices[i-1]; labels[i] = labels[i-1];
        }
        dists[pos] = dist; indices[pos] = index; labels[pos] = label;
    }
}

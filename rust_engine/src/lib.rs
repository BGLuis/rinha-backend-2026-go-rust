use std::ffi::CStr;
use std::fs::File;
use std::os::raw::c_char;
use std::slice;
use memmap2::{Mmap, MmapOptions};
use std::arch::x86_64::*;

static mut DATASET_MMAP: Option<Mmap> = None;
static mut CENTROIDS: Option<&'static [[f32; 16]]> = None;
static mut BBOXES: Option<&'static [[i16; 32]]> = None;
static mut OFFSETS: Option<&'static [u32]> = None;
static mut SIZES: Option<&'static [u32]> = None;
static mut NUM_CLUSTERS: usize = 0;

#[no_mangle]
pub extern "C" fn init_engine(path_ptr: *const c_char) -> i32 {
    unsafe {
        let c_str = CStr::from_ptr(path_ptr);
        let path = match c_str.to_str() { Ok(s) => s, Err(_) => return -1 };
        let file = match File::open(path) { Ok(f) => f, Err(_) => return -2 };
        let mmap = match MmapOptions::new().map(&file) { Ok(m) => m, Err(_) => return -3 };
        
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
pub unsafe extern "C" fn search_vector(query_ptr: *const f32) -> i32 {
    let q_in = slice::from_raw_parts(query_ptr, 14);
    let mut q = [0.0f32; 16];
    q[0..14].copy_from_slice(q_in);

    // lossless i16 query
    let mut q_i16 = [0i16; 16];
    for d in 0..14 { q_i16[d] = (q[d] * 10000.0).round() as i16; }
    let q_i16_simd = _mm256_loadu_si256(q_i16.as_ptr() as *const __m256i);

    let q_low = _mm256_loadu_ps(q.as_ptr());
    let q_high = _mm256_loadu_ps(q.as_ptr().add(8));

    let centroids = CENTROIDS.unwrap();
    let bboxes = BBOXES.unwrap();
    let num_k = NUM_CLUSTERS;
    let mmap_ptr = DATASET_MMAP.as_ref().unwrap().as_ptr();
    let offsets = OFFSETS.unwrap();
    let sizes = SIZES.unwrap();

    let mut top_dists_u64 = [u64::MAX; 5];
    let mut top_labels = [0u32; 5];
    let mut top_indices = [u32::MAX; 5];

    // 1. Find nearest centroid
    let mut best_ki = 0;
    let mut best_kd2 = f32::MAX;
    for (ki, c) in centroids.iter().enumerate() {
        let c_low = _mm256_loadu_ps(c.as_ptr());
        let c_high = _mm256_loadu_ps(c.as_ptr().add(8));
        let d_low = _mm256_sub_ps(q_low, c_low);
        let d_high = _mm256_sub_ps(q_high, c_high);
        let mut sq = _mm256_mul_ps(d_low, d_low);
        sq = _mm256_fmadd_ps(d_high, d_high, sq);
        let d2 = hsum_ps_avx(sq);
        if d2 < best_kd2 {
            best_kd2 = d2;
            best_ki = ki;
        }
    }

    // 2. Scan best cluster
    scan_cluster(best_ki, q_i16_simd, mmap_ptr, offsets, sizes, &mut top_dists_u64, &mut top_indices, &mut top_labels);

    // 3. Exhaustive search with BBox Pruning (Integer Exact)
    for ki in 0..num_k {
        if ki == best_ki { continue; }
        
        let bbox = &bboxes[ki];
        let min_d2_u64 = dist_to_bbox_i16(q_i16_simd, bbox);
        
        if min_d2_u64 <= top_dists_u64[4] {
            scan_cluster(ki, q_i16_simd, mmap_ptr, offsets, sizes, &mut top_dists_u64, &mut top_indices, &mut top_labels);
        }
    }

    top_labels.iter().sum::<u32>() as i32
}

#[inline(always)]
unsafe fn scan_cluster(ki: usize, q_i16: __m256i, mmap_ptr: *const u8, offsets: &[u32], sizes: &[u32], top_dists_u64: &mut [u64; 5], top_indices: &mut [u32; 5], top_labels: &mut [u32; 5]) {
    let n = sizes[ki] as usize;
    let base_ptr = mmap_ptr.add(offsets[ki] as usize);
    
    for i in 0..n {
        let p_ptr = base_ptr.add(i * 36) as *const __m256i;
        _mm_prefetch(base_ptr.add((i + 8) * 36) as *const i8, _MM_HINT_T0);

        let p_i16 = _mm256_loadu_si256(p_ptr);
        let d2 = dist_sq_i16_avx(q_i16, p_i16);

        if d2 <= top_dists_u64[4] {
            let meta_ptr = (p_ptr as *const u8).add(32) as *const u32;
            let meta = *meta_ptr;
            let label = (meta >> 31) & 1;
            let index = meta & 0x7FFFFFFF;
            
            if d2 < top_dists_u64[4] || index < top_indices[4] {
                insert_top5(d2, index, label, top_dists_u64, top_indices, top_labels);
            }
        }
    }
}

#[inline(always)]
unsafe fn dist_to_bbox_i16(q_i16: __m256i, bbox: &[i16; 32]) -> u64 {
    let b_min = _mm256_loadu_si256(bbox.as_ptr() as *const __m256i);
    let b_max = _mm256_loadu_si256(bbox.as_ptr().add(16) as *const __m256i);
    let zero = _mm256_setzero_si256();

    // d1 = min - q; d2 = q - max
    let d1 = _mm256_sub_epi16(b_min, q_i16);
    let d2 = _mm256_sub_epi16(q_i16, b_max);
    
    // diff = max(0, max(d1, d2))
    let diff = _mm256_max_epi16(zero, _mm256_max_epi16(d1, d2));
    
    dist_sq_i16_avx(diff, zero) // Reuse sum-of-squares logic
}

#[inline(always)]
unsafe fn dist_sq_i16_avx(q: __m256i, p: __m256i) -> u64 {
    let diff = _mm256_sub_epi16(q, p);
    let sq_i32 = _mm256_madd_epi16(diff, diff);
    
    let x128 = _mm_add_epi32(_mm256_extracti128_si256(sq_i32, 1), _mm256_castsi256_si128(sq_i32));
    let x64 = _mm_add_epi32(x128, _mm_srli_si128(x128, 8));
    let x32 = _mm_add_epi32(x64, _mm_srli_si128(x64, 4));
    _mm_cvtsi128_si32(x32) as u64
}

#[inline(always)]
unsafe fn hsum_ps_avx(v: __m256) -> f32 {
    let x128 = _mm_add_ps(_mm256_extractf128_ps(v, 1), _mm256_castps256_ps128(v));
    let x64 = _mm_add_ps(x128, _mm_movehl_ps(x128, x128));
    let x32 = _mm_add_ss(x64, _mm_shuffle_ps(x64, x64, 0x55));
    _mm_cvtss_f32(x32)
}

#[inline(always)]
fn insert_top5(dist: u64, index: u32, label: u32, dists: &mut [u64; 5], indices: &mut [u32; 5], labels: &mut [u32; 5]) {
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

use std::ffi::CStr;
use std::fs::File;
use std::os::raw::c_char;
use std::slice;
use memmap2::{Mmap, MmapOptions};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

static mut DATASET_MMAP: Option<Mmap> = None;
static mut CENTROIDS: Option<&'static [[f32; 16]]> = None;
static mut BBOXES: Option<&'static [[f32; 32]]> = None;
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

        let bboxes_ptr = mmap.as_ptr().add(64 + k * 64) as *const [f32; 32];
        BBOXES = Some(slice::from_raw_parts(bboxes_ptr, k));

        let offsets_ptr = mmap.as_ptr().add(64 + k * 64 + k * 128) as *const u32;
        OFFSETS = Some(slice::from_raw_parts(offsets_ptr, k));

        let sizes_ptr = mmap.as_ptr().add(64 + k * 64 + k * 128 + k * 4) as *const u32;
        SIZES = Some(slice::from_raw_parts(sizes_ptr, k));

        DATASET_MMAP = Some(mmap);
        0
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn hsum_ps_avx(v: __m256) -> f32 {
    let v_low = _mm256_castps256_ps128(v);
    let v_high = _mm256_extractf128_ps(v, 1);
    let v_add = _mm_add_ps(v_low, v_high);
    let v_shuf = _mm_shuffle_ps(v_add, v_add, 0b10110001);
    let v_add2 = _mm_add_ps(v_add, v_shuf);
    let v_shuf2 = _mm_movehl_ps(v_shuf, v_add2);
    let v_add3 = _mm_add_ps(v_add2, v_shuf2);
    _mm_cvtss_f32(v_add3)
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

    #[cfg(target_arch = "x86_64")]
    {
        let q_v = _mm256_loadu_ps(q.as_ptr());
        let q_v2 = _mm256_loadu_ps(q.as_ptr().add(8));
        for ki in 0..n_centroids {
            let p_v = _mm256_loadu_ps(centroids[ki].as_ptr());
            let diff = _mm256_sub_ps(q_v, p_v);
            let mut sq = _mm256_mul_ps(diff, diff);
            let p_v2 = _mm256_loadu_ps(centroids[ki].as_ptr().add(8));
            let diff2 = _mm256_sub_ps(q_v2, p_v2);
            sq = _mm256_fmadd_ps(diff2, diff2, sq);
            centroid_dists[ki] = (hsum_ps_avx(sq), ki);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        for ki in 0..n_centroids {
            let mut d2 = 0.0f32;
            for d in 0..14 {
                let diff = q[d] - centroids[ki][d];
                d2 += diff * diff;
            }
            centroid_dists[ki] = (d2, ki);
        }
    }

    let sub = &mut centroid_dists[0..n_centroids];
    let nprobe = if force_deep == 1 { 192 } else { 64 };
    
    if nprobe > 0 && nprobe <= sub.len() {
        sub.select_nth_unstable_by(nprobe - 1, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let top_nprobe = &mut sub[0..nprobe];
        top_nprobe.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    } else {
        sub.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    for i in 0..nprobe {
        if i >= sub.len() { break; }
        let ki = sub[i].1;
        if min_dist_to_bbox_f32(&q, &bboxes[ki]) > top_dists[4] { continue; }
        scan_cluster_aos(ki, &q, mmap_ptr, offsets, sizes, &mut top_dists, &mut top_indices, &mut top_labels);
    }

    top_labels.iter().sum::<u32>() as i32
}

#[inline(always)]
unsafe fn min_dist_to_bbox_f32(q: &[f32; 16], bbox: &[f32; 32]) -> f32 {
    let mut dist2 = 0f32;
    for d in 0..14 {
        let b_min = bbox[d];
        let b_max = bbox[d+16];
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
unsafe fn scan_cluster_aos(ki: usize, q: &[f32; 16], mmap_ptr: *const u8, offsets: &[u32], sizes: &[u32], top_dists: &mut [f32; 5], top_indices: &mut [u32; 5], top_labels: &mut [u32; 5]) {
    let n = sizes[ki] as usize;
    if n == 0 { return; }
    let base_ptr = mmap_ptr.add(offsets[ki] as usize) as *const f32;

    #[cfg(target_arch = "x86_64")]
    {
        let q_v0 = _mm256_loadu_ps(q.as_ptr());
        let q_v1 = _mm256_loadu_ps(q.as_ptr().add(8));
        
        let mask = _mm256_castsi256_ps(_mm256_set_epi32(0, 0, -1, -1, -1, -1, -1, -1));

        for i in 0..n {
            let record_ptr = base_ptr.add(i * 16);
            let r_v0 = _mm256_loadu_ps(record_ptr);
            let diff0 = _mm256_sub_ps(q_v0, r_v0);
            let mut sq = _mm256_mul_ps(diff0, diff0);
            
            let r_v1 = _mm256_loadu_ps(record_ptr.add(8));
            let diff1 = _mm256_sub_ps(q_v1, r_v1);
            let mut sq1 = _mm256_mul_ps(diff1, diff1);
            
            sq1 = _mm256_and_ps(sq1, mask);
            
            sq = _mm256_add_ps(sq, sq1);
            let d2 = hsum_ps_avx(sq);

            if d2 <= top_dists[4] {
                let meta_f32 = *record_ptr.add(15);
                let meta = meta_f32.to_bits();
                let index = meta & 0x7FFFFFFF;
                let label = (meta >> 31) & 1;
                insert_top5(d2, index, label, top_dists, top_indices, top_labels);
            }
        }
    }
    
    #[cfg(not(target_arch = "x86_64"))]
    {
        for i in 0..n {
            let record_ptr = base_ptr.add(i * 16);
            let mut d2 = 0.0f32;
            for d in 0..14 {
                let diff = q[d] - *record_ptr.add(d);
                d2 += diff * diff;
            }
            if d2 <= top_dists[4] {
                let meta_f32 = *record_ptr.add(15);
                let meta = meta_f32.to_bits();
                let index = meta & 0x7FFFFFFF;
                let label = (meta >> 31) & 1;
                insert_top5(d2, index, label, top_dists, top_indices, top_labels);
            }
        }
    }
}

#[inline(always)]
fn insert_top5(d2: f32, index: u32, label: u32, top_dists: &mut [f32; 5], top_indices: &mut [u32; 5], top_labels: &mut [u32; 5]) {
    if d2 < top_dists[0] {
        top_dists[4] = top_dists[3]; top_indices[4] = top_indices[3]; top_labels[4] = top_labels[3];
        top_dists[3] = top_dists[2]; top_indices[3] = top_indices[2]; top_labels[3] = top_labels[2];
        top_dists[2] = top_dists[1]; top_indices[2] = top_indices[1]; top_labels[2] = top_labels[1];
        top_dists[1] = top_dists[0]; top_indices[1] = top_indices[0]; top_labels[1] = top_labels[0];
        top_dists[0] = d2; top_indices[0] = index; top_labels[0] = label;
    } else if d2 < top_dists[1] {
        top_dists[4] = top_dists[3]; top_indices[4] = top_indices[3]; top_labels[4] = top_labels[3];
        top_dists[3] = top_dists[2]; top_indices[3] = top_indices[2]; top_labels[3] = top_labels[2];
        top_dists[2] = top_dists[1]; top_indices[2] = top_indices[1]; top_labels[2] = top_labels[1];
        top_dists[1] = d2; top_indices[1] = index; top_labels[1] = label;
    } else if d2 < top_dists[2] {
        top_dists[4] = top_dists[3]; top_indices[4] = top_indices[3]; top_labels[4] = top_labels[3];
        top_dists[3] = top_dists[2]; top_indices[3] = top_indices[2]; top_labels[3] = top_labels[2];
        top_dists[2] = d2; top_indices[2] = index; top_labels[2] = label;
    } else if d2 < top_dists[3] {
        top_dists[4] = top_dists[3]; top_indices[4] = top_indices[3]; top_labels[4] = top_labels[3];
        top_dists[3] = d2; top_indices[3] = index; top_labels[3] = label;
    } else if d2 < top_dists[4] {
        top_dists[4] = d2; top_indices[4] = index; top_labels[4] = label;
    }
}

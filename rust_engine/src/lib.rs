use std::ffi::CStr;
use std::os::raw::c_char;
use std::slice;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

static mut MMAP_PTR: Option<*const u8> = None;
static mut CENTROIDS: Option<&'static [[i16; 16]]> = None;
static mut BBOXES: Option<&'static [[i16; 32]]> = None;
static mut OFFSETS: Option<&'static [u32]> = None;
static mut NUM_BLOCKS: Option<&'static [u32]> = None;
static mut NUM_CLUSTERS: usize = 0;

#[no_mangle]
pub extern "C" fn init_engine(path_ptr: *const c_char) -> i32 {
    unsafe {
        let c_str = CStr::from_ptr(path_ptr);
        let path = match c_str.to_str() { Ok(s) => s, Err(_) => return -1 };
        
        let fd = libc::open(path.as_ptr() as *const libc::c_char, libc::O_RDONLY);
        if fd < 0 { return -2; }
        
        let mut stat = std::mem::zeroed();
        if libc::fstat(fd, &mut stat) < 0 { return -3; }
        let len = stat.st_size as usize;
        
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED | libc::MAP_POPULATE,
            fd,
            0,
        );
        if ptr == libc::MAP_FAILED { return -4; }
        libc::close(fd);

        libc::mlock(ptr, len);

        let header = slice::from_raw_parts(ptr as *const u32, 16);
        if header[0] != 0x4E495452 { return -5; }
        if header[1] != 5 { return -6; } // expect version 5

        let k = header[2] as usize;
        NUM_CLUSTERS = k;

        let centroids_ptr = ptr.add(64) as *const [i16; 16];
        CENTROIDS = Some(slice::from_raw_parts(centroids_ptr, k));

        let bboxes_ptr = ptr.add(64 + k * 32) as *const [i16; 32];
        BBOXES = Some(slice::from_raw_parts(bboxes_ptr, k));

        let offsets_ptr = ptr.add(64 + k * 32 + k * 64) as *const u32;
        OFFSETS = Some(slice::from_raw_parts(offsets_ptr, k));

        let num_blocks_ptr = ptr.add(64 + k * 32 + k * 64 + k * 4) as *const u32;
        NUM_BLOCKS = Some(slice::from_raw_parts(num_blocks_ptr, k));

        MMAP_PTR = Some(ptr as *const u8);
        0
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn dist_avx2_i16(q_vec: __m256i, b_ptr: *const i16) -> i32 {
    let b = _mm256_loadu_si256(b_ptr as *const __m256i);
    let diff = _mm256_sub_epi16(q_vec, b);
    let mask = _mm256_set_epi16(0, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);
    let masked_diff = _mm256_and_si256(diff, mask);
    
    let sums = _mm256_madd_epi16(masked_diff, masked_diff);
    
    let sum1 = _mm256_add_epi32(sums, _mm256_shuffle_epi32(sums, 0b01001110));
    let sum2 = _mm256_add_epi32(sum1, _mm256_shuffle_epi32(sum1, 0b10110001));
    let sum_low = _mm256_castsi256_si128(sum2);
    let sum_high = _mm256_extracti128_si256(sum2, 1);
    let sum3 = _mm_add_epi32(sum_low, sum_high);
    _mm_cvtsi128_si32(sum3)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn min_dist_to_bbox_avx2(q_vec: __m256i, bbox_ptr: *const i16) -> i32 {
    let min_v = _mm256_loadu_si256(bbox_ptr as *const __m256i);
    let max_v = _mm256_loadu_si256((bbox_ptr.add(16)) as *const __m256i);
    
    let diff_min = _mm256_sub_epi16(min_v, q_vec);
    let diff_max = _mm256_sub_epi16(q_vec, max_v);
    
    let zero = _mm256_setzero_si256();
    let p_min = _mm256_max_epi16(diff_min, zero);
    let p_max = _mm256_max_epi16(diff_max, zero);
    
    let diff = _mm256_or_si256(p_min, p_max);
    let mask = _mm256_set_epi16(0, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);
    let masked_diff = _mm256_and_si256(diff, mask);
    
    let sums = _mm256_madd_epi16(masked_diff, masked_diff);
    let sum1 = _mm256_add_epi32(sums, _mm256_shuffle_epi32(sums, 0b01001110));
    let sum2 = _mm256_add_epi32(sum1, _mm256_shuffle_epi32(sum1, 0b10110001));
    let sum_low = _mm256_castsi256_si128(sum2);
    let sum_high = _mm256_extracti128_si256(sum2, 1);
    let sum3 = _mm_add_epi32(sum_low, sum_high);
    _mm_cvtsi128_si32(sum3)
}

#[inline(always)]
unsafe fn scan_cluster_aos(
    ki: usize,
    q_vec: __m256i,
    q_i16: &[i16; 16],
    mmap_ptr: *const u8,
    offsets: &[u32],
    num_blocks: &[u32],
    top_dists: &mut [i32; 5],
    top_indices: &mut [u32; 5],
    top_labels: &mut [u32; 5],
) {
    let offset = offsets[ki] as usize;
    let blocks_count = num_blocks[ki] as usize;
    let mut ptr = mmap_ptr.add(offset);
    
    for _ in 0..blocks_count {
        let bbox = slice::from_raw_parts(ptr as *const i16, 32);
        let num_vectors = *(ptr.add(64) as *const u32) as usize;
        ptr = ptr.add(96);
        
        if min_dist_to_bbox_avx2(q_vec, bbox.as_ptr()) < top_dists[4] {
            let mut v_ptr = ptr as *const i16;
            for _ in 0..num_vectors {
                let d = dist_avx2_i16(q_vec, v_ptr);
                if d < top_dists[4] {
                    let m0 = *v_ptr.add(14) as u16 as u32;
                    let m1 = *v_ptr.add(15) as u16 as u32;
                    let meta = m0 | (m1 << 16);
                    let idx = meta & 0x7FFFFFFF;
                    let label = meta >> 31;

                    let mut pos = 4;
                    while pos > 0 && d < top_dists[pos - 1] {
                        top_dists[pos] = top_dists[pos - 1];
                        top_indices[pos] = top_indices[pos - 1];
                        top_labels[pos] = top_labels[pos - 1];
                        pos -= 1;
                    }
                    top_dists[pos] = d;
                    top_indices[pos] = idx;
                    top_labels[pos] = label;
                }
                v_ptr = v_ptr.add(16);
            }
        }
        ptr = ptr.add(num_vectors * 32);
    }
}

static FEATURE_WEIGHTS: [f32; 16] = [
    1.0038165, 0.665417, 0.8668326, 0.5379362, 0.5, 0.3, 0.3701757, 1.0, 1.2, 1.2648705, 0.81239825, 1.051987, 0.8247206, 2.0315619, 0.0, 0.0,
];

#[no_mangle]
pub unsafe extern "C" fn search_vector(query_ptr: *const f32, force_deep: i32) -> i32 {
    let q_in = slice::from_raw_parts(query_ptr, 14);
    let mut q_i16 = [0i16; 16];
    for i in 0..14 {
        q_i16[i] = (q_in[i] * 10000.0).round() as i16;
    }
    
    let centroids = match CENTROIDS { Some(c) => c, None => return 0 };
    let bboxes = match BBOXES { Some(b) => b, None => return 0 };
    let mmap_ptr = match MMAP_PTR { Some(m) => m, None => return 0 };
    let offsets = OFFSETS.unwrap();
    let num_blocks = NUM_BLOCKS.unwrap();
    let num_k = NUM_CLUSTERS;

    let q_vec = _mm256_loadu_si256(q_i16.as_ptr() as *const __m256i);

    let mut centroid_dists = [(0i32, 0usize); 8192];
    let n_centroids = std::cmp::min(num_k, 8192);
    
    for i in 0..n_centroids {
        centroid_dists[i] = (dist_avx2_i16(q_vec, centroids[i].as_ptr()), i);
    }

    let sub = &mut centroid_dists[0..n_centroids];
    sub.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    let mut top_dists = [i32::MAX; 5];
    let mut top_indices = [0u32; 5];
    let mut top_labels = [0u32; 5];

    let nprobe = n_centroids;
    
    sub.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    for i in 0..nprobe {
        let ki = sub[i].1;
        if min_dist_to_bbox_avx2(q_vec, bboxes[ki].as_ptr()) >= top_dists[4] { continue; }
        scan_cluster_aos(ki, q_vec, &q_i16, mmap_ptr, offsets, num_blocks, &mut top_dists, &mut top_indices, &mut top_labels);
    }

    let mut frauds = 0;
    for i in 0..5 {
        if top_dists[i] != i32::MAX && top_labels[i] == 1 {
            frauds += 1;
        }
    }

    frauds
}

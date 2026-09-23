use std::ffi::CStr;
use std::os::raw::c_char;
use std::slice;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[repr(align(64))]
#[derive(Clone, Copy)]
struct AlignedBBox([i16; 32]);

#[derive(Copy, Clone)]
#[repr(C)]
struct SuperDist {
    dist: i32,
    s: u8,
}

#[derive(Copy, Clone)]
#[repr(C)]
struct ChildDist {
    dist: i32,
    ki: u16,
}

static mut MMAP_PTR: Option<*const u8> = None;
static mut CENTROIDS: Option<&'static [[i16; 16]]> = None;
static mut BBOXES: Option<&'static [[i16; 32]]> = None;
static mut OFFSETS: Option<&'static [u32]> = None;
static mut NUM_BLOCKS: Option<&'static [u32]> = None;

static mut SUPER_CENTROIDS: [[i16; 16]; 91] = [[0; 16]; 91];
static mut SUPER_BBOXES: [AlignedBBox; 91] = [AlignedBBox([0; 32]); 91];
static mut SUPER_OFFSETS: [u16; 92] = [0; 92];
static mut SUPER_CHILDREN: [u16; 4096] = [0; 4096];

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

        libc::madvise(ptr, len, libc::MADV_WILLNEED);
        libc::mlock(ptr, len);

        let header = slice::from_raw_parts(ptr as *const u32, 16);
        if header[0] != 0x4E495452 { return -5; }
        if header[1] != 5 { return -6; } // expect version 5

        let k = header[2] as usize;
        assert_eq!(k, 4096, "Expected 4096 clusters");

        let centroids_ptr = ptr.add(64) as *const [i16; 16];
        let centroids = slice::from_raw_parts(centroids_ptr, k);

        let bboxes_ptr = ptr.add(64 + k * 32) as *const [i16; 32];
        BBOXES = Some(slice::from_raw_parts(bboxes_ptr, k));

        let offsets_ptr = ptr.add(64 + k * 32 + k * 64) as *const u32;
        OFFSETS = Some(slice::from_raw_parts(offsets_ptr, k));

        let num_blocks_ptr = ptr.add(64 + k * 32 + k * 64 + k * 4) as *const u32;
        NUM_BLOCKS = Some(slice::from_raw_parts(num_blocks_ptr, k));

        MMAP_PTR = Some(ptr as *const u8);

        // Build Exact Flat BVH (Super-BBoxes) for SIMD-friendly pruning
        let num_super = 91;
        let mut super_centroids = vec![[0i16; 16]; num_super];
        for i in 0..num_super {
            super_centroids[i] = centroids[i * (k / num_super)];
        }
        
        let mask = _mm256_set_epi16(0, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);
        let mut assignment = vec![0; k];
        for _ in 0..5 {
            let mut sums = vec![[0i32; 16]; num_super];
            let mut counts = vec![0; num_super];
            for i in 0..k {
                let mut best_d = i32::MAX;
                let mut best_s = 0;
                let c_vec = _mm256_loadu_si256(centroids[i].as_ptr() as *const __m256i);
                for s in 0..num_super {
                    let d = dist_avx2_i16(c_vec, super_centroids[s].as_ptr(), mask);
                    if d < best_d {
                        best_d = d;
                        best_s = s;
                    }
                }
                assignment[i] = best_s;
                counts[best_s] += 1;
                for j in 0..16 {
                    sums[best_s][j] += centroids[i][j] as i32;
                }
            }
            for s in 0..num_super {
                if counts[s] > 0 {
                    for j in 0..16 {
                        super_centroids[s][j] = (sums[s][j] / counts[s]) as i16;
                    }
                }
            }
        }
        
        let mut counts = [0u16; 91];
        for i in 0..k {
            counts[assignment[i]] += 1;
        }

        let mut offsets = [0u16; 92];
        for s in 0..91 {
            assert!(counts[s] <= 256, "Super cluster {} has {} children (> 256)", s, counts[s]);
            offsets[s + 1] = offsets[s] + counts[s];
        }

        let mut cur_pos = offsets;
        let mut children = [0u16; 4096];
        for i in 0..k {
            let s = assignment[i];
            children[cur_pos[s] as usize] = i as u16;
            cur_pos[s] += 1;
        }

        let bboxes = BBOXES.unwrap();
        let mut super_bboxes = [AlignedBBox([0i16; 32]); 91];
        for s in 0..91 {
            let start = offsets[s] as usize;
            let end = offsets[s + 1] as usize;
            if start == end { continue; }
            let first = children[start] as usize;
            let mut s_bbox = bboxes[first];
            for &idx in &children[start + 1..end] {
                let ki = idx as usize;
                for j in 0..16 {
                    s_bbox[j] = std::cmp::min(s_bbox[j], bboxes[ki][j]);
                    s_bbox[j + 16] = std::cmp::max(s_bbox[j + 16], bboxes[ki][j + 16]);
                }
            }
            super_bboxes[s] = AlignedBBox(s_bbox);
        }

        CENTROIDS = Some(centroids);
        let mut super_cent_arr = [[0i16; 16]; 91];
        for s in 0..91 {
            super_cent_arr[s] = super_centroids[s];
        }
        SUPER_CENTROIDS = super_cent_arr;

        SUPER_BBOXES = super_bboxes;
        SUPER_OFFSETS = offsets;
        SUPER_CHILDREN = children;

        // Warmup: Synthetic searches to train BPU and L3 Cache
        let dummy_query = [0i16; 16];
        for _ in 0..1000 {
            search_vector(dummy_query.as_ptr(), 0);
        }

        0
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn dist_avx2_i16(q_vec: __m256i, b_ptr: *const i16, mask: __m256i) -> i32 {
    let b = _mm256_loadu_si256(b_ptr as *const __m256i);
    let diff = _mm256_sub_epi16(q_vec, b);
    let masked_diff = _mm256_and_si256(diff, mask);
    let sums = _mm256_madd_epi16(masked_diff, masked_diff);
    
    let hi128 = _mm256_extracti128_si256(sums, 1);
    let lo128 = _mm256_castsi256_si128(sums);
    let s1 = _mm_add_epi32(hi128, lo128);
    let s2 = _mm_add_epi32(s1, _mm_shuffle_epi32(s1, 0x4E));
    let s3 = _mm_add_epi32(s2, _mm_shuffle_epi32(s2, 0xB1));
    _mm_cvtsi128_si32(s3)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn min_dist_to_bbox_avx2(q_vec: __m256i, bbox_ptr: *const i16, mask: __m256i) -> i32 {
    let min_v = _mm256_loadu_si256(bbox_ptr as *const __m256i);
    let max_v = _mm256_loadu_si256((bbox_ptr.add(16)) as *const __m256i);
    
    let diff_min = _mm256_sub_epi16(min_v, q_vec);
    let diff_max = _mm256_sub_epi16(q_vec, max_v);
    
    let zero = _mm256_setzero_si256();
    let p_min = _mm256_max_epi16(diff_min, zero);
    let p_max = _mm256_max_epi16(diff_max, zero);
    
    let diff = _mm256_or_si256(p_min, p_max);
    let masked_diff = _mm256_and_si256(diff, mask);
    
    let sq = _mm256_madd_epi16(masked_diff, masked_diff);
    
    let hi128 = _mm256_extracti128_si256(sq, 1);
    let lo128 = _mm256_castsi256_si128(sq);
    let s1 = _mm_add_epi32(hi128, lo128);
    let s2 = _mm_add_epi32(s1, _mm_shuffle_epi32(s1, 0x4E));
    let s3 = _mm_add_epi32(s2, _mm_shuffle_epi32(s2, 0xB1));

    _mm_cvtsi128_si32(s3)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn dist_2_avx2_i16(q_vec: __m256i, b0_ptr: *const i16, b1_ptr: *const i16, mask: __m256i) -> (i32, i32) {
    let b0 = _mm256_loadu_si256(b0_ptr as *const __m256i);
    let b1 = _mm256_loadu_si256(b1_ptr as *const __m256i);
    let diff0 = _mm256_sub_epi16(q_vec, b0);
    let diff1 = _mm256_sub_epi16(q_vec, b1);
    let masked0 = _mm256_and_si256(diff0, mask);
    let masked1 = _mm256_and_si256(diff1, mask);
    let sq0 = _mm256_madd_epi16(masked0, masked0);
    let sq1 = _mm256_madd_epi16(masked1, masked1);

    let h1 = _mm256_hadd_epi32(sq0, sq1);
    let h2 = _mm256_hadd_epi32(h1, h1);
    let lo = _mm256_castsi256_si128(h2);
    let hi = _mm256_extracti128_si256(h2, 1);
    let sum = _mm_add_epi32(lo, hi);
    let d0 = _mm_cvtsi128_si32(sum);
    let d1 = _mm_extract_epi32(sum, 1);
    (d0, d1)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn min_dist_2_bboxes_avx2(
    q_vec: __m256i,
    b0_ptr: *const i16,
    b1_ptr: *const i16,
    mask: __m256i,
    zero: __m256i,
) -> (i32, i32) {
    let min0 = _mm256_loadu_si256(b0_ptr as *const __m256i);
    let min1 = _mm256_loadu_si256(b1_ptr as *const __m256i);
    let max0 = _mm256_loadu_si256((b0_ptr.add(16)) as *const __m256i);
    let max1 = _mm256_loadu_si256((b1_ptr.add(16)) as *const __m256i);

    let diff_min0 = _mm256_sub_epi16(min0, q_vec);
    let diff_min1 = _mm256_sub_epi16(min1, q_vec);
    let diff_max0 = _mm256_sub_epi16(q_vec, max0);
    let diff_max1 = _mm256_sub_epi16(q_vec, max1);

    let p_min0 = _mm256_max_epi16(diff_min0, zero);
    let p_min1 = _mm256_max_epi16(diff_min1, zero);
    let p_max0 = _mm256_max_epi16(diff_max0, zero);
    let p_max1 = _mm256_max_epi16(diff_max1, zero);

    let diff0 = _mm256_or_si256(p_min0, p_max0);
    let diff1 = _mm256_or_si256(p_min1, p_max1);
    let masked0 = _mm256_and_si256(diff0, mask);
    let masked1 = _mm256_and_si256(diff1, mask);

    let sq0 = _mm256_madd_epi16(masked0, masked0);
    let sq1 = _mm256_madd_epi16(masked1, masked1);

    let h1 = _mm256_hadd_epi32(sq0, sq1);
    let h2 = _mm256_hadd_epi32(h1, h1);
    let lo = _mm256_castsi256_si128(h2);
    let hi = _mm256_extracti128_si256(h2, 1);
    let sum = _mm_add_epi32(lo, hi);
    let d0 = _mm_cvtsi128_si32(sum);
    let d1 = _mm_extract_epi32(sum, 1);

    (d0, d1)
}

#[inline(always)]
unsafe fn insert_top(
    d: i32,
    v_ptr: *const i16,
    t_d: &mut [i32; 5],
    t_i: &mut [u32; 5],
    t_l: &mut [u32; 5],
) {
    let meta = *(v_ptr.add(14) as *const u32);
    let idx = meta & 0x7FFFFFFF;
    let label = meta >> 31;

    let mut k = 3;
    while d < t_d[k] {
        t_d[k + 1] = t_d[k];
        t_i[k + 1] = t_i[k];
        t_l[k + 1] = t_l[k];
        if k == 0 { break; }
        k -= 1;
    }
    let ins = if d < t_d[0] { 0 } else { k + 1 };
    t_d[ins] = d;
    t_i[ins] = idx;
    t_l[ins] = label;
}

#[inline(always)]
unsafe fn scan_cluster_aos(
    ki: usize,
    q_vec: __m256i,
    mmap_ptr: *const u8,
    offsets: &[u32],
    num_blocks: &[u32],
    top_dists: &mut [i32; 5],
    top_indices: &mut [u32; 5],
    top_labels: &mut [u32; 5],
    mask: __m256i,
) {
    let offset = offsets[ki] as usize;
    let blocks_count = num_blocks[ki] as usize;
    let mut ptr = mmap_ptr.add(offset);
    
    let mut t_d = *top_dists;
    let mut t_i = *top_indices;
    let mut t_l = *top_labels;

    for _ in 0..blocks_count {
        let bbox = slice::from_raw_parts(ptr as *const i16, 32);
        let num_vectors = *(ptr.add(64) as *const u32) as usize;
        ptr = ptr.add(96);
        
        if min_dist_to_bbox_avx2(q_vec, bbox.as_ptr(), mask) < t_d[4] {
            let mut v_ptr = ptr as *const i16;
            let mut remaining = num_vectors;
            while remaining >= 2 {
                let b0 = _mm256_loadu_si256(v_ptr as *const __m256i);
                let b1 = _mm256_loadu_si256(v_ptr.add(16) as *const __m256i);

                let diff0 = _mm256_sub_epi16(q_vec, b0);
                let diff1 = _mm256_sub_epi16(q_vec, b1);

                let masked0 = _mm256_and_si256(diff0, mask);
                let masked1 = _mm256_and_si256(diff1, mask);

                let sq0 = _mm256_madd_epi16(masked0, masked0);
                let sq1 = _mm256_madd_epi16(masked1, masked1);

                let h1 = _mm256_hadd_epi32(sq0, sq1);
                let h2 = _mm256_hadd_epi32(h1, h1);
                let lo = _mm256_castsi256_si128(h2);
                let hi = _mm256_extracti128_si256(h2, 1);
                let sum = _mm_add_epi32(lo, hi);
                let d0 = _mm_cvtsi128_si32(sum);
                let d1 = _mm_extract_epi32(sum, 1);

                if d0 < t_d[4] {
                    insert_top(d0, v_ptr, &mut t_d, &mut t_i, &mut t_l);
                }
                if d1 < t_d[4] {
                    insert_top(d1, v_ptr.add(16), &mut t_d, &mut t_i, &mut t_l);
                }

                v_ptr = v_ptr.add(32);
                remaining -= 2;
            }
            if remaining == 1 {
                let d = dist_avx2_i16(q_vec, v_ptr, mask);
                if d < t_d[4] {
                    insert_top(d, v_ptr, &mut t_d, &mut t_i, &mut t_l);
                }
            }
        }
        ptr = ptr.add(num_vectors * 32);
    }
    
    *top_dists = t_d;
    *top_indices = t_i;
    *top_labels = t_l;
}

#[no_mangle]
pub unsafe extern "C" fn search_vector(query_ptr: *const i16, _force_deep: i32) -> i32 {
    let q_vec = _mm256_loadu_si256(query_ptr as *const __m256i);
    let mask = _mm256_set_epi16(0, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);

    let centroids = match CENTROIDS { Some(c) => c, None => return 0 };
    let bboxes = match BBOXES { Some(b) => b, None => return 0 };
    let mmap_ptr = match MMAP_PTR { Some(m) => m, None => return 0 };
    let offsets = match OFFSETS { Some(o) => o, None => return 0 };
    let num_blocks = match NUM_BLOCKS { Some(n) => n, None => return 0 };

    let mut top_dists = [i32::MAX; 5];
    let mut top_indices = [0u32; 5];
    let mut top_labels = [0u32; 5];

    // Pre-seed top_dists with the nearest centroid cluster (unrolled 2x)
    let mut best_s = 0;
    let mut best_s_dist = i32::MAX;
    let mut s_idx = 0;
    while s_idx + 1 < 91 {
        let (d0, d1) = dist_2_avx2_i16(q_vec, SUPER_CENTROIDS[s_idx].as_ptr(), SUPER_CENTROIDS[s_idx + 1].as_ptr(), mask);
        if d0 < best_s_dist {
            best_s_dist = d0;
            best_s = s_idx;
        }
        if d1 < best_s_dist {
            best_s_dist = d1;
            best_s = s_idx + 1;
        }
        s_idx += 2;
    }
    if s_idx < 91 {
        let d = dist_avx2_i16(q_vec, SUPER_CENTROIDS[s_idx].as_ptr(), mask);
        if d < best_s_dist {
            best_s = s_idx;
        }
    }

    let start_seed = SUPER_OFFSETS[best_s] as usize;
    let end_seed = SUPER_OFFSETS[best_s + 1] as usize;
    let mut best_ki = SUPER_CHILDREN[start_seed] as usize;
    let mut best_ki_dist = i32::MAX;
    let count_seed = end_seed - start_seed;
    let mut ki_idx = 0;
    while ki_idx + 1 < count_seed {
        let ki0 = SUPER_CHILDREN[start_seed + ki_idx] as usize;
        let ki1 = SUPER_CHILDREN[start_seed + ki_idx + 1] as usize;
        let (d0, d1) = dist_2_avx2_i16(q_vec, centroids[ki0].as_ptr(), centroids[ki1].as_ptr(), mask);
        if d0 < best_ki_dist {
            best_ki_dist = d0;
            best_ki = ki0;
        }
        if d1 < best_ki_dist {
            best_ki_dist = d1;
            best_ki = ki1;
        }
        ki_idx += 2;
    }
    if ki_idx < count_seed {
        let ki = SUPER_CHILDREN[start_seed + ki_idx] as usize;
        let d = dist_avx2_i16(q_vec, centroids[ki].as_ptr(), mask);
        if d < best_ki_dist {
            best_ki = ki;
        }
    }

    scan_cluster_aos(best_ki, q_vec, mmap_ptr, offsets, num_blocks, &mut top_dists, &mut top_indices, &mut top_labels, mask);

    let zero = _mm256_setzero_si256();
    let mut s = 0;
    let mut active_super = [SuperDist { dist: 0, s: 0 }; 91];
    let mut num_active_super = 0;

    while s + 1 < 91 {
        let (d0, d1) = min_dist_2_bboxes_avx2(
            q_vec,
            SUPER_BBOXES[s].0.as_ptr(),
            SUPER_BBOXES[s + 1].0.as_ptr(),
            mask,
            zero,
        );
        if d0 < top_dists[4] {
            active_super[num_active_super] = SuperDist { dist: d0, s: s as u8 };
            num_active_super += 1;
        }
        if d1 < top_dists[4] {
            active_super[num_active_super] = SuperDist { dist: d1, s: (s + 1) as u8 };
            num_active_super += 1;
        }
        s += 2;
    }
    if s < 91 {
        let d = min_dist_to_bbox_avx2(q_vec, SUPER_BBOXES[s].0.as_ptr(), mask);
        if d < top_dists[4] {
            active_super[num_active_super] = SuperDist { dist: d, s: s as u8 };
            num_active_super += 1;
        }
    }

    // Sort ONLY the active super bboxes (typically 1 to 4 instead of 91)
    let super_sub = &mut active_super[0..num_active_super];
    super_sub.sort_unstable_by_key(|e| e.dist);

    let mut active_children = [ChildDist { dist: 0, ki: 0 }; 256];

    for s_entry in super_sub {
        if s_entry.dist >= top_dists[4] { break; }
        let s_idx = s_entry.s as usize;
        
        let start = SUPER_OFFSETS[s_idx] as usize;
        let end = SUPER_OFFSETS[s_idx + 1] as usize;
        let count = end - start;
        let mut i = 0;
        let mut num_active_children = 0;

        while i + 1 < count {
            let ki0 = SUPER_CHILDREN[start + i] as usize;
            let ki1 = SUPER_CHILDREN[start + i + 1] as usize;
            let (d0, d1) = min_dist_2_bboxes_avx2(
                q_vec,
                bboxes[ki0].as_ptr(),
                bboxes[ki1].as_ptr(),
                mask,
                zero,
            );
            if d0 < top_dists[4] {
                active_children[num_active_children] = ChildDist { dist: d0, ki: ki0 as u16 };
                num_active_children += 1;
            }
            if d1 < top_dists[4] {
                active_children[num_active_children] = ChildDist { dist: d1, ki: ki1 as u16 };
                num_active_children += 1;
            }
            i += 2;
        }
        if i < count {
            let ki = SUPER_CHILDREN[start + i] as usize;
            let d = min_dist_to_bbox_avx2(q_vec, bboxes[ki].as_ptr(), mask);
            if d < top_dists[4] {
                active_children[num_active_children] = ChildDist { dist: d, ki: ki as u16 };
                num_active_children += 1;
            }
        }
        
        let child_sub = &mut active_children[0..num_active_children];
        child_sub.sort_unstable_by_key(|e| e.dist);
        
        for ch in child_sub {
            if ch.dist >= top_dists[4] { break; }
            let ki = ch.ki as usize;
            if ki != best_ki {
                scan_cluster_aos(ki, q_vec, mmap_ptr, offsets, num_blocks, &mut top_dists, &mut top_indices, &mut top_labels, mask);
            }
        }
    }

    let mut frauds = 0;
    for i in 0..5 {
        if top_dists[i] != i32::MAX && top_labels[i] == 1 {
            frauds += 1;
        }
    }

    frauds
}

use crate::progress::Bar;
use crate::voxel_map::BlockingVoxelMap as VoxelMap;
use atomic_counter::{AtomicCounter, RelaxedCounter};
use crossbeam_utils::thread;
use rustc_hash::FxHashMap;

pub enum WeightResult {
    Maxima,
    Interier(usize),
    Boundary(Vec<f64>),
}

/// Steps in the density grid, from point p, following the gradient.
///
/// This should be called from [`weight()`].
///
/// Note: This function will deadlock if the points above it have no associated
/// maxima in [`VoxelMap.voxel_map`].
///
/// * `p`: The point from which to step.
/// * `density`: The reference [`Grid`].
/// * `weight_map`: An [`Arc`] wrapped [`VoxelMap`] for tracking the maxima.
///
/// ### Returns:
/// [`WeightResult`]: The type of point `p` is (maxima, interior, boundary) and
/// the relevant data for each type.
///
/// # Examples
/// ```
/// use bader::voxel_map::VoxelMap;
/// use bader::methods::{WeightResult, weight_step};
///
/// // Intialise the reference density, setting index 34 to 0. for easy maths.
/// let density = (0..64).map(|rho| if rho != 34 { rho  as f64 } else {0.})
///                      .collect::<Vec<f64>>();
/// let voxel_map = VoxelMap::new([4, 4, 4],
///                               [[3.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 3.0]],
///                               [0.0, 0.0, 0.0]);
/// // The highest gradient between point, p = 33, and it's neighbours, with
/// // periodic boundary conditions, is with point p = 61.
///
/// // to avoid deadlock let's store maxima for all the values above us and
/// // store as either 61 or 62 to make the current point a boundary.
/// for (i, p) in [37, 45, 49].iter().enumerate() {
///     voxel_map.maxima_store(*p, 62 - (i as isize) % 2);
/// }
/// let weight = match weight_step(33, &density, &voxel_map, 1E-8) {
///     WeightResult::Boundary(weights) => weights,
///     _ => Vec::with_capacity(0),
/// };
/// assert_eq!(weight, vec![62.625, 61.375])
/// ```
pub fn weight_step(p: isize,
                   density: &[f64],
                   voxel_map: &VoxelMap,
                   weight_tolerance: f64)
                   -> WeightResult {
    let control = density[p as usize];
    let grid = &voxel_map.grid;
    let mut t_sum = 0.;
    let mut weights = FxHashMap::<usize, f64>::default();
    // colllect the shift and distances and iterate over them.
    for (shift, alpha) in grid.voronoi.vectors.iter().zip(&grid.voronoi.alphas)
    {
        let pt = grid.voronoi_shift(p, shift);
        let charge_diff = density[pt as usize] - control;
        if charge_diff > 0. {
            // calculate the gradient and add any weights to the HashMap.
            let rho = charge_diff * alpha;
            let maxima = voxel_map.maxima_get(pt);
            match maxima.cmp(&-1) {
                std::cmp::Ordering::Less => {
                    let point_weights = voxel_map.weight_get(maxima);
                    for maxima_weight in point_weights.iter() {
                        let maxima = *maxima_weight as usize;
                        let w = maxima_weight - maxima as f64;
                        let weight = weights.entry(maxima).or_insert(0.);
                        *weight += w * rho;
                    }
                }
                std::cmp::Ordering::Greater => {
                    let weight = weights.entry(maxima as usize).or_insert(0.);
                    *weight += rho;
                }
                std::cmp::Ordering::Equal => (),
            }
            t_sum += rho;
        }
    }
    // Sort the weights, if they exist, by the most probable.
    match weights.len().cmp(&1) {
        std::cmp::Ordering::Greater => {
            let mut total = 0.;
            let mut weights = weights.into_iter()
                                     .filter_map(|(maxima, weight)| {
                                         let weight = weight / t_sum;
                                         if weight > weight_tolerance {
                                             total += weight;
                                             Some((maxima, weight))
                                         } else {
                                             None
                                         }
                                     })
                                     .collect::<Vec<(usize, f64)>>();
            if weights.len() > 1 {
                weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                // re-adjust the weights
                let mut weights =
                    weights.iter()
                           .map(|(maxima, w)| *maxima as f64 + w / total)
                           .collect::<Vec<f64>>();
                weights.shrink_to_fit();
                WeightResult::Boundary(weights)
            } else {
                WeightResult::Interier(weights[0].0)
            }
        }
        std::cmp::Ordering::Equal => {
            WeightResult::Interier(*weights.keys().next().unwrap())
        }
        std::cmp::Ordering::Less => WeightResult::Maxima,
    }
}

/// Finds the maxima associated with the current point, p.
///
/// Note: This function will deadlock if the points above it have no associated
/// maxima in [`VoxelMap.voxel_map`].
///
/// * `p`: The point from which to step.
/// * `density`: The reference [`Grid`].
/// * `weight_map`: An [`Arc`] wrapped [`VoxelMap`] for tracking the maxima.
///
/// ### Returns:
/// `(isize, Vec::<f64>::with_capacity(14))`: The maxima associated with the
/// point, p, and it's associated weights.
///
/// # Examples
/// ```
/// use bader::voxel_map::VoxelMap;
/// use bader::methods::weight;
///
/// // Intialise the reference density, setting index 34 to 0. for easy maths.
/// let density = (0..64).map(|rho| if rho != 34 { rho  as f64 } else {0.})
///                      .collect::<Vec<f64>>();
/// let voxel_map = VoxelMap::new([4, 4, 4],
///                               [[3.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 3.0]],
///                               [0.0, 0.0, 0.0]);
/// // The highest gradient between point, p = 33, and it's neighbours, with
/// // periodic boundary conditions, is with point p = 61.
///
/// // to avoid deadlock let's store maxima for all the values above us and
/// // store as either 61 or 62 to make the current point a boundary.
/// for (i, p) in [37, 45, 49].iter().enumerate() {
///     voxel_map.maxima_store(*p, 62 - (i as isize) % 2);
/// }
/// weight(33, &density, &voxel_map, 1E-8);
/// assert_eq!(voxel_map.weight_get(-2), &vec![62.625, 61.375]);
/// ```
pub fn weight(density: &[f64],
              voxel_map: &VoxelMap,
              index: &[usize],
              progress_bar: Bar,
              threads: usize,
              vacuum_index: usize,
              weight_tolerance: f64) {
    let counter = RelaxedCounter::new(0);
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|_| loop {
                 let p = {
                     let i = counter.inc();
                     if i >= vacuum_index {
                         break;
                     };
                     index[i] as isize
                 };
                 match weight_step(p, density, voxel_map, weight_tolerance) {
                     WeightResult::Maxima => voxel_map.maxima_store(p, p),
                     WeightResult::Interier(maxima) => {
                         voxel_map.maxima_store(p, maxima as isize);
                     }
                     WeightResult::Boundary(weights) => {
                         let i = {
                             let mut weight = voxel_map.lock();
                             let i = weight.len();
                             (*weight).push(weights);
                             i
                         };
                         voxel_map.weight_store(p, i);
                     }
                 }
                 progress_bar.tick();
             });
        }
    }).unwrap();
    {
        let mut weights = voxel_map.lock();
        weights.shrink_to_fit();
    }
}

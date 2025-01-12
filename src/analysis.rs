use crate::atoms::Atoms;
use crate::grid::Grid;
use crate::progress::Bar;
use crate::utils;
use crate::voxel_map::NonBlockingVoxelMap as VoxelMap;
use anyhow::{Context, Result};
use crossbeam_utils::thread;
use rustc_hash::FxHashSet;

/// A type to simplify the result of charge summing functions
type ChargeSumResult = Result<(Vec<Vec<f64>>, Vec<f64>, Vec<f64>)>;

/// The Errors Associated with the [`Analysis`] structure.
pub enum AnalysisError {
    /// Not finding index for supplied maxima.
    NotMaxima,
}

/// Make Errors printable.
impl std::fmt::Display for AnalysisError {
    /// Match the error and write the text associated with matched error.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMaxima => f.write_str(
                "Error: Attempted to look up non-maxima in maxima index.",
            ),
        }
    }
}

/// Make errors unwrapable
impl std::fmt::Debug for AnalysisError {
    /// Match the error and write the text associated with matched error.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMaxima => f.write_str(
                "Error: Attempted to look up non-maxima in maxima index.",
            ),
        }
    }
}

/// Calculates the distance between a maxima and its nearest atom.
/// Chunk represents a collection of bader maxima positions withing the density
/// array.
fn maxima_to_atom(chunk: &[isize],
                  atoms: &Atoms,
                  grid: &Grid,
                  progress_bar: &Bar)
                  -> Result<(Vec<usize>, Vec<f64>)> {
    let chunk_size = chunk.len();
    // create vectors for storing the assigned atom and distance for each maxima
    let mut ass_atom = Vec::with_capacity(chunk_size);
    let mut min_dist = Vec::with_capacity(chunk_size);
    for m in chunk.iter() {
        // convert the point first to cartesian, then to the reduced basis
        let m_cartesian = grid.to_cartesian(*m as isize);
        let m_reduced_cartesian = atoms.reduced_lattice.to_reduced(m_cartesian);
        let mut atom_num = 0;
        let mut min_distance = f64::INFINITY;
        // go through each atom in the reduced basis and shift in each
        // reduced direction, save the atom with the shortest distance
        for (i, atom) in atoms.reduced_positions.iter().enumerate() {
            for atom_shift in
                atoms.reduced_lattice.cartesian_shift_matrix.iter()
            {
                let distance = {
                    (m_reduced_cartesian[0]
                                        - (atom[0] + atom_shift[0]))
                                                                    .powi(2)
                                       + (m_reduced_cartesian[1]
                                          - (atom[1] + atom_shift[1]))
                                                                      .powi(2)
                                       + (m_reduced_cartesian[2]
                                          - (atom[2] + atom_shift[2]))
                                                                      .powi(2)
                };
                if distance < min_distance {
                    min_distance = distance;
                    atom_num = i;
                }
            }
        }
        // remember to square root the distance
        ass_atom.push(atom_num);
        min_dist.push(min_distance.powf(0.5));
        progress_bar.tick()
    }
    Ok((ass_atom, min_dist))
}

/// Assign the Bader maxima to the nearest atom.
/// Threading will split the slice of maxima into chunks and operate on each
/// chunk in parallel.
pub fn assign_maxima(maxima: &[isize],
                     atoms: &Atoms,
                     grid: &Grid,
                     threads: usize,
                     progress_bar: Bar)
                     -> Result<(Vec<usize>, Vec<f64>)> {
    let mut assigned_atom = vec![0; maxima.len()];
    let mut minimum_distance = vec![0.0; maxima.len()];
    let pbar = &progress_bar;
    // this is basically a thread handling function for running the
    // maxima_to_atom function
    match threads.cmp(&1) {
        std::cmp::Ordering::Greater => {
            let chunk_size =
                (maxima.len() / threads) + (maxima.len() % threads).min(1);
            thread::scope(|s| {
                let spawned_threads =
                    maxima.chunks(chunk_size)
                          .enumerate()
                          .map(|(index, chunk)| {
                              s.spawn(move |_| {
                                  match maxima_to_atom(chunk, atoms, grid, pbar) {
                                      Ok(result) => (result, index),
                                      _ => panic!("Failed to match maxima to atom"),
                                  }
                               })
                          })
                          .collect::<Vec<_>>();
                for thread in spawned_threads {
                    if let Ok(((ass_atom, min_dist), chunk_index)) =
                        thread.join()
                    {
                        // is this required? is the collection of handles not
                        // already sorted like this, is it possible to join as
                        // they finish?
                        let i = chunk_index * chunk_size;
                        assigned_atom.splice(i..(i + ass_atom.len()), ass_atom);
                        minimum_distance.splice(i..(i + min_dist.len()),
                                                min_dist);
                    } else {
                        panic!("Failed to join thread in assign maxima.")
                    };
                }
            }).unwrap();
        }
        _ => {
            let (ass_atom, min_dist) =
                maxima_to_atom(maxima, atoms, grid, pbar).context("Failed to assign maxima to atom.")?;
            assigned_atom = ass_atom;
            minimum_distance = min_dist;
        }
    }
    Ok((assigned_atom, minimum_distance))
}

// I don't like having two functions here there is so much duplicated code
// how can this be fixed?

/// Sum the densities for when the maxima are Bader volumes and not atoms.
/// Chunk is a slice of the voxel map.
fn sum_densities_bader(chunk: &[isize],
                       densities: &[Vec<f64>],
                       atoms_map: &[usize],
                       atoms: &Atoms,
                       voxel_map: &VoxelMap,
                       index: usize,
                       progress_bar: &Bar)
                       -> ChargeSumResult {
    let mut bader_charge = vec![vec![0.0; densities.len()]; atoms_map.len()];
    let mut bader_volume = vec![0.0; atoms_map.len()];
    let mut surface_distance = vec![f64::INFINITY; atoms.positions.len()];
    chunk.iter()
         .enumerate()
         .try_for_each(|(voxel_index, voxel)| -> Result<()> {
             let p = index * chunk.len() + voxel_index;
             match voxel.cmp(&-1) {
                 // If we are at an interior point sum the charge and volume.
                 std::cmp::Ordering::Greater => {
                     bader_charge[*voxel as usize].iter_mut()
                                                  .zip(densities)
                                                  .for_each(|(bc, density)| {
                                                      *bc += density[p];
                                                  });
                     bader_volume[*voxel as usize] += 1.0;
                 }
                 // If instead it is a weight then also check if it is at a boundary between atoms.
                 std::cmp::Ordering::Less => {
                     let weights = voxel_map.weight_get(*voxel);
                     let maxima = weights[0] as usize;
                     let atom_number = atoms_map[maxima];
                     let mut is_atom_boundary = false;
                     for w in weights.iter() {
                         let maxima = *w as usize;
                         let weight = w - maxima as f64;
                         if atom_number != atoms_map[maxima] {
                             is_atom_boundary = true;
                         }
                         bader_charge[maxima].iter_mut()
                                             .zip(densities)
                                             .for_each(|(bc, density)| {
                                                 *bc += density[p] * weight;
                                             });
                         bader_volume[maxima] += weight;
                     }
                     if is_atom_boundary {
                         let minimum_distance =
                             &mut surface_distance[atom_number];
                         let p_cartesian =
                             voxel_map.grid.to_cartesian(p as isize);
                         let p_cartesian = utils::dot(p_cartesian,
                                                      voxel_map.grid
                                                               .voxel_lattice
                                                               .to_cartesian);
                         let mut p_lll_fractional =
                             utils::dot(p_cartesian,
                                        atoms.reduced_lattice.to_fractional);
                         for f in &mut p_lll_fractional {
                             *f = f.rem_euclid(1.);
                         }
                         let p_lll_cartesian =
                             utils::dot(p_lll_fractional,
                                        atoms.reduced_lattice.to_cartesian);
                         let atom = atoms.reduced_positions[atom_number];
                         for atom_shift in
                             atoms.reduced_lattice.cartesian_shift_matrix.iter()
                         {
                             let distance = {
                                 (p_lll_cartesian[0]
                                  - (atom[0] + atom_shift[0]))
                                                              .powi(2)
                                 + (p_lll_cartesian[1]
                                    - (atom[1] + atom_shift[1]))
                                                                .powi(2)
                                 + (p_lll_cartesian[2]
                                    - (atom[2] + atom_shift[2]))
                                                                .powi(2)
                             };
                             if distance < *minimum_distance {
                                 *minimum_distance = distance;
                             }
                         }
                     }
                 }
                 // Vacuum
                 std::cmp::Ordering::Equal => (),
             }
             progress_bar.tick();
             Ok(())
         })
         .context("Iterating through a chunk of the voxel map.")?;
    Ok((bader_charge, bader_volume, surface_distance))
}

/// Sum the densities for when the maxima are Bader atoms.
/// Chunk is a slice of the voxel map.
fn sum_densities_atom(chunk: &[isize],
                      densities: &[Vec<f64>],
                      atoms: &Atoms,
                      voxel_map: &VoxelMap,
                      index: usize,
                      progress_bar: &Bar)
                      -> ChargeSumResult {
    let mut bader_charge =
        vec![vec![0.0; densities.len()]; atoms.positions.len()];
    let mut bader_volume = vec![0.0; atoms.positions.len()];
    let mut surface_distance = vec![f64::INFINITY; atoms.positions.len()];
    chunk.iter()
         .enumerate()
         .try_for_each(|(voxel_index, voxel)| -> Result<()> {
             let p = index * chunk.len() + voxel_index;
             match voxel.cmp(&-1) {
                 // If we are at an interior point sum the charge and volume.
                 std::cmp::Ordering::Greater => {
                     bader_charge[*voxel as usize].iter_mut()
                                                  .zip(densities)
                                                  .for_each(|(bc, density)| {
                                                      *bc += density[p];
                                                  });
                     bader_volume[*voxel as usize] += 1.0;
                 }
                 // If instead it is a weight then it is at a boundary between atoms.
                 std::cmp::Ordering::Less => {
                     let weights = voxel_map.weight_get(*voxel);
                     let atom_number = weights[0] as usize;
                     let minimum_distance = &mut surface_distance[atom_number];
                     let p_cartesian = voxel_map.grid.to_cartesian(p as isize);
                     let p_cartesian =
                         utils::dot(p_cartesian,
                                    voxel_map.grid.voxel_lattice.to_cartesian);
                     let mut p_lll_fractional =
                         utils::dot(p_cartesian,
                                    atoms.reduced_lattice.to_fractional);
                     for f in &mut p_lll_fractional {
                         *f = f.rem_euclid(1.);
                     }
                     let p_lll_cartesian = utils::dot(p_lll_fractional,
                                                      atoms.reduced_lattice
                                                           .to_cartesian);
                     let atom = atoms.reduced_positions[atom_number];
                     for atom_shift in
                         atoms.reduced_lattice.cartesian_shift_matrix.iter()
                     {
                         let distance = {
                             (p_lll_cartesian[0]
                                  - (atom[0] + atom_shift[0]))
                                                              .powi(2)
                                 + (p_lll_cartesian[1]
                                    - (atom[1] + atom_shift[1]))
                                                                .powi(2)
                                 + (p_lll_cartesian[2]
                                    - (atom[2] + atom_shift[2]))
                                                                .powi(2)
                         };
                         if distance < *minimum_distance {
                             *minimum_distance = distance;
                         }
                     }
                     for w in weights.iter() {
                         let maxima = *w as usize;
                         let weight = w - maxima as f64;
                         bader_charge[maxima].iter_mut()
                                             .zip(densities)
                                             .for_each(|(bc, density)| {
                                                 *bc += density[p] * weight;
                                             });
                         bader_volume[maxima] += weight;
                     }
                 }
                 // Vacuum
                 std::cmp::Ordering::Equal => (),
             }
             progress_bar.tick();
             Ok(())
         })
         .context("Iterating through a chunk of the voxel map.")?;
    Ok((bader_charge, bader_volume, surface_distance))
}

/// Sums the densities of each Bader volume.
pub fn sum_bader_densities(densities: &[Vec<f64>],
                           voxel_map: &VoxelMap,
                           atoms: &Atoms,
                           atoms_map: Option<&[usize]>,
                           threads: usize,
                           maxima_len: usize,
                           progress_bar: Bar)
                           -> ChargeSumResult {
    let pbar = &progress_bar;
    // Only spawn threads if more than 1 thread is required.
    // This minimises overhead?
    let (mut bader_charge, mut bader_volume, mut surface_distance) =
        match threads.cmp(&1) {
            std::cmp::Ordering::Greater => {
                let mut surface_distance =
                    vec![f64::INFINITY; atoms.positions.len()];
                let mut bader_charge =
                    vec![vec![0.0; densities.len()]; maxima_len];
                let mut bader_volume = vec![0.0; maxima_len];
                // Calculate the size of the vector to be passed to each thread.
                let chunk_size = (voxel_map.voxel_map.len() / threads)
                                 + (voxel_map.voxel_map.len() % threads).min(1);
                thread::scope(|s| {
                let spawned_threads = voxel_map.voxel_map
                                               .chunks(chunk_size)
                                               .enumerate()
                                               .map(|(index, chunk)| {
                                                   if let Some(am) = atoms_map {
                                                       s.spawn(move |_| {
                                          match sum_densities_bader(chunk,
                                                                    densities,
                                                                    am,
                                                                    atoms,
                                                                    voxel_map,
                                                                    index,
                                                                    pbar)
                                  {
                                      Ok(result) => result,
                                      _ => panic!("Unable to sum densities."),
                                  }
                                      })
                                                   } else {
                                                       s.spawn(move |_| {
                                          match sum_densities_atom(chunk,
                                                                   densities,
                                                                   atoms,
                                                                   voxel_map,
                                                                   index,
                                                                   pbar)
                                  {
                                      Ok(result) => result,
                                      _ => panic!("Unable to sum densities."),
                                  }
                                      })
                                                   }
                                               })
                                               .collect::<Vec<_>>();
                // Join each thread and collect the results.
                // If one thread terminates before the other this is not operated on first.
                // Either use the sorted index to remove vacuum from the summation or
                // find a way to operate on finshed threads first (ideally both).
                for thread in spawned_threads {
                    if let Ok((tmp_bc, tmp_bv, tmp_sd)) = thread.join() {
                        for (bc, density) in
                            bader_charge.iter_mut().zip(tmp_bc.into_iter())
                        {
                            bc.iter_mut()
                              .zip(density.iter())
                              .for_each(|(a, b)| {
                                  *a += b;
                              });
                        }
                        bader_volume.iter_mut()
                                    .zip(tmp_bv.into_iter())
                                    .for_each(|(a, b)| {
                                        *a += b;
                                    });
                        surface_distance.iter_mut()
                                        .zip(tmp_sd.into_iter())
                                        .for_each(|(a, b)| {
                                            *a = a.min(b);
                                        });
                    } else {
                        panic!("Unable to join thread in sum_bader_densities.")
                    };
                }
            }).unwrap();
                // The distance isn't square rooted in the calcation of distance to save time.
                // As we need to filter out the infinite distances (atoms with no assigned maxima)
                // we can square root here also.
                surface_distance.iter_mut()
                            .for_each(|d| {
                                match (*d).partial_cmp(&f64::INFINITY) {
                                    Some(std::cmp::Ordering::Less) => *d = d.powf(0.5),
                                    _ => *d = 0.0,
                                }
                            });
                (bader_charge, bader_volume, surface_distance)
            }
            _ => {
                    match atoms_map {
                    Some(am) => sum_densities_bader(&voxel_map.voxel_map,
                                  densities,
                                  am,
                                  atoms,
                                  voxel_map,
                                  0,
                                  pbar).context("Unable to sum bader densities")?,
                    None => sum_densities_atom(&voxel_map.voxel_map,
                                               densities,
                                               atoms,
                                               voxel_map,
                                               0,
                                               pbar).context("Unable to sum bader densities")?
                }
            }
        };
    // The distance isn't square rooted in the calcation of distance to save time.
    // As we need to filter out the infinite distances (atoms with no assigned maxima)
    // we can square root here also.
    for bc in bader_charge.iter_mut() {
        bc.iter_mut().for_each(|a| {
                         *a *= voxel_map.grid.voxel_lattice.volume;
                     });
    }
    bader_volume.iter_mut().for_each(|a| {
                               *a *= voxel_map.grid.voxel_lattice.volume;
                           });
    surface_distance.iter_mut().for_each(|d| {
                                   match (*d).partial_cmp(&f64::INFINITY) {
                                       Some(std::cmp::Ordering::Less) => {
                                           *d = d.powf(0.5)
                                       }
                                       _ => *d = 0.0,
                                   }
                               });
    Ok((bader_charge, bader_volume, surface_distance))
}

/// Sums the densities for each atom.
pub fn sum_atoms_densities(bader_charge: &[Vec<f64>],
                           bader_volume: &[f64],
                           atoms_map: &[usize],
                           atoms_len: usize)
                           -> Result<(Vec<Vec<f64>>, Vec<f64>)> {
    let mut atoms_density = vec![vec![0.0; bader_charge[0].len()]; atoms_len];
    let mut atoms_volume = vec![0.0; atoms_len];
    bader_charge.iter()
                .zip(bader_volume)
                .zip(atoms_map)
                .for_each(|((bc, bv), am)| {
                    atoms_density[*am].iter_mut()
                                      .zip(bc)
                                      .for_each(|(ad, d)| *ad += d);
                    atoms_volume[*am] += bv;
                });
    Ok((atoms_density, atoms_volume))
}

// The unwrap here is necessary for lifetime resolution
/// Create nearest neighbour matrix from the atoms with shared voxels.
#[allow(clippy::unnecessary_unwrap)]
pub fn nearest_neighbours(voxel_map: &VoxelMap,
                          atoms_map: Option<&[usize]>,
                          n_atoms: usize)
                          -> Result<Vec<Vec<bool>>> {
    let mut m_nn = vec![vec![false; n_atoms]; n_atoms];
    let maxima_map: Box<dyn Iterator<Item = FxHashSet<usize>>> =
        if atoms_map.is_some() {
            Box::new(voxel_map.weight_map.iter().map(|weights| {
                                                weights.iter()
                                                       .map(|w| atoms_map.unwrap()[*w as usize])
                                                       .collect()
                                            }))
        } else {
            Box::new(voxel_map.weight_map.iter().map(|weights| {
                                                    weights.iter()
                                                           .map(|w| *w as usize)
                                                           .collect()
                                                }))
        };
    // as both indices are added then the removal of the maxima from the set is
    // valid, this also helps with not operating on single occupancy sets from
    // voxel maps with non-atomic maxima
    maxima_map.for_each(|mut maxima_set| {
                  let maximas: Vec<usize> =
                      maxima_set.iter().copied().collect();
                  maximas.iter().for_each(|maxima| {
                                    maxima_set.remove(maxima);
                                    maxima_set.iter().for_each(|m| {
                                                         m_nn[*maxima][*m] =
                                                             true;
                                                         m_nn[*m][*maxima] =
                                                             true;
                                                     });
                                });
              });
    Ok(m_nn)
}

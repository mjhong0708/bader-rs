use crate::atoms::Atoms;
use crate::grid::Grid;
use crate::progress::Bar;
use crate::utils;
use crate::voxel_map::{NonBlockingVoxelMap as VoxelMap, Voxel};
use anyhow::{bail, Context, Result};
use crossbeam_utils::thread;
use rustc_hash::FxHashMap;

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

fn maxima_to_atom(chunk: &[isize],
                  atoms: &Atoms,
                  grid: &Grid,
                  progress_bar: &Bar)
                  -> Result<(Vec<usize>, Vec<f64>)> {
    let chunk_size = chunk.len();
    let mut ass_atom = Vec::with_capacity(chunk_size);
    let mut min_dist = Vec::with_capacity(chunk_size);
    for m in chunk.iter() {
        let maxima_cartesian = grid.to_cartesian(*m as isize);
        let maxima_cartesian =
            { utils::dot(maxima_cartesian, grid.voxel_lattice.to_cartesian) };
        let mut maxima_lll_fractional =
            utils::dot(maxima_cartesian, atoms.reduced_lattice.to_fractional);
        for f in &mut maxima_lll_fractional {
            *f = f.rem_euclid(1.);
        }
        let maxima_lll_cartesian = utils::dot(maxima_lll_fractional,
                                              atoms.reduced_lattice
                                                   .to_cartesian);
        let mut atom_num = 0;
        let mut min_distance = f64::INFINITY;
        for (i, atom) in atoms.reduced_positions.iter().enumerate() {
            for atom_shift in
                atoms.reduced_lattice.cartesian_shift_matrix.iter()
            {
                let distance = {
                    (maxima_lll_cartesian[0]
                                        - (atom[0] + atom_shift[0]))
                                                                    .powi(2)
                                       + (maxima_lll_cartesian[1]
                                          - (atom[1] + atom_shift[1]))
                                                                      .powi(2)
                                       + (maxima_lll_cartesian[2]
                                          - (atom[2] + atom_shift[2]))
                                                                      .powi(2)
                };
                if distance < min_distance {
                    min_distance = distance;
                    atom_num = i;
                }
            }
        }
        ass_atom.push(atom_num);
        min_dist.push(min_distance.powf(0.5));
        progress_bar.tick()
    }
    Ok((ass_atom, min_dist))
}

/// Assign a Bader maxima to the nearest atom.
pub fn assign_maxima(maxima: &[isize],
                     atoms: &Atoms,
                     grid: &Grid,
                     threads: usize,
                     progress_bar: Bar)
                     -> Result<(Vec<usize>, Vec<f64>)> {
    let mut assigned_atom = vec![0; maxima.len()];
    let mut minimum_distance = vec![0.0; maxima.len()];
    let pbar = &progress_bar;
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

fn sum_densities(chunk: &[isize],
                 densities: &[Vec<f64>],
                 atoms_map: &FxHashMap<usize, usize>,
                 atoms: &Atoms,
                 voxel_map: &VoxelMap,
                 index: usize,
                 progress_bar: &Bar)
                 -> Result<(Vec<Vec<f64>>, Vec<f64>, Vec<f64>)> {
    let mut bader_charge =
        vec![vec![0.0; voxel_map.maxima_map.len()]; densities.len()];
    let mut bader_volume = vec![0.0; voxel_map.maxima_map.len()];
    let mut surface_distance = vec![f64::INFINITY; atoms.positions.len()];
    let maxima_map = &voxel_map.maxima_map;
    chunk.iter().enumerate().for_each(|(voxel_index, voxel)| {
        let p = index * chunk.len() + voxel_index;
        match voxel.cmp(&-1) {
            std::cmp::Ordering::Greater => {
                let maxima = maxima_map.get(&(*voxel as usize)).unwrap();
                for (i, rho) in bader_charge.iter_mut().enumerate() {
                    rho[*maxima] += densities[i][p];
                }
                bader_volume[*maxima] += 1.0;
            }
            std::cmp::Ordering::Less => {
                let weights = voxel_map.weight_get(*voxel);
                let atom_number = atoms_map.get(&(weights[0] as usize)).unwrap();
                let mut is_atom_boundary = false;
                for w in weights.iter() {
                    let m = *w as usize;
                    let weight = w - m as f64;
                    let maxima = maxima_map.get(&m).unwrap();
                    if atom_number != atoms_map.get(&m).unwrap() {
                        is_atom_boundary = true;
                    }
                    for (i, rho) in bader_charge.iter_mut().enumerate() {
                        rho[*maxima] += densities[i][p];
                    }
                    bader_volume[*maxima] += weight;
                }
                if is_atom_boundary {
                    let minimum_distance = &mut surface_distance[*atom_number];
                    let p_cartesian = voxel_map.grid.to_cartesian(p as isize);
                    let p_cartesian = utils::dot(p_cartesian,
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
                    let atom = atoms.reduced_positions[*atom_number];
                    for atom_shift in
                        atoms.reduced_lattice.cartesian_shift_matrix.iter()
                    {
                        let distance = {
                            (p_lll_cartesian[0] - (atom[0] + atom_shift[0]))
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
            },
            std::cmp::Ordering::Equal => (),
        }
        progress_bar.tick();
    });
    Ok((bader_charge, bader_volume, surface_distance))
}

pub fn sum_bader_densities(densities: &[Vec<f64>],
                           voxel_map: &VoxelMap,
                           atoms: &Atoms,
                           atoms_map: &FxHashMap<usize, usize>,
                           threads: usize,
                           progress_bar: Bar)
                           -> Result<(Vec<Vec<f64>>, Vec<f64>, Vec<f64>)> {
    let pbar = &progress_bar;
    // Only spawn threads if more than 1 thread is required.
    // This minimises overhead?
    match threads.cmp(&1) {
        std::cmp::Ordering::Greater => {
            let mut surface_distance =
                vec![f64::INFINITY; atoms.positions.len()];
            let mut bader_charge =
                vec![vec![0.0; voxel_map.maxima_map.len()]; densities.len()];
            let mut bader_volume = vec![0.0; voxel_map.maxima_map.len()];
            // Calculate the size of the vector to be passed to each thread.
            let chunk_size = (voxel_map.voxel_map.len() / threads)
                             + (voxel_map.voxel_map.len() % threads).min(1);
            thread::scope(|s| {
                let spawned_threads =
                    voxel_map.voxel_map
                             .chunks(chunk_size)
                             .enumerate()
                             .map(|(index, chunk)| {
                                 s.spawn(move |_| match sum_densities(chunk,
                                                                      densities,
                                                                      atoms_map,
                                                                      atoms,
                                                                      voxel_map,
                                                                      index,
                                                                      pbar)
                                  {
                                      Ok(result) => result,
                                      _ => panic!("Unable to sum densities."),
                                  })
                             })
                             .collect::<Vec<_>>();
                // Join each thread and collect the results.
                // If one thread terminates before the other this is not operated on first.
                // Either use the sorted index to remove vacuum from the summation or
                // find a way to operate on finshed threads first (ideally both).
                for thread in spawned_threads {
                    if let Ok((tmp_bc, tmp_bv, tmp_sd)) = thread.join() {
                        for (i, density) in tmp_bc.iter().enumerate() {
                            bader_charge[i].iter_mut()
                                           .zip(density.iter())
                                           .for_each(|(a, b)| {
                                               *a += b * voxel_map.grid
                                                                  .voxel_lattice
                                                                  .volume;
                                           });
                        }
                        bader_volume.iter_mut()
                                    .zip(tmp_bv.into_iter())
                                    .for_each(|(a, b)| {
                                        *a += b * voxel_map.grid
                                                           .voxel_lattice
                                                           .volume;
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
                                if let Some(std::cmp::Ordering::Less) = (*d).partial_cmp(&f64::INFINITY) {
                                    *d = d.powf(0.5)
                                } else {
                                    *d = 0.0
                                }
                            });
            Ok((bader_charge, bader_volume, surface_distance))
        }
        _ => {
            let (bader_charge, bader_volume, mut surface_distance) =
                sum_densities(&voxel_map.voxel_map,
                              densities,
                              atoms_map,
                              atoms,
                              voxel_map,
                              0,
                              pbar).context("Unable to sum bader densities")?;
            // The distance isn't square rooted in the calcation of distance to save time.
            // As we need to filter out the infinite distances (atoms with no assigned maxima)
            // we can square root here also.
            surface_distance.iter_mut()
                            .for_each(|d| {
                                if let Some(std::cmp::Ordering::Less) = (*d).partial_cmp(&f64::INFINITY) {
                                    *d = d.powf(0.5)
                                } else {
                                    *d = 0.0
                                }
                            });
            Ok((bader_charge, bader_volume, surface_distance))
        }
    }
}

/// Structure for analysing a partitioned [`VoxelMap`].
pub struct Analysis {
    /// The atom assigned to each bader maxima.
    pub assigned_atom: Vec<usize>,
    /// The minimum distance to each atom from the bader maxima.
    pub minimum_distance: Vec<f64>,
    /// The minimum distance to the surface of the bader volume.
    pub surface_distance: Vec<f64>,
    /// Stores the index of the Bader maxima in [`self.bader_maxima`].
    maxima_index: FxHashMap<usize, usize>,
    /// List of all the maxima within the [`VoxelMap`]
    pub bader_maxima: Vec<usize>,
    /// The charge (and spin) associated with with each maxima. Takes the form
    /// vec![vec![f64; [`self.bader_maxima`].len()]; Number of Densities].
    pub bader_charge: Vec<Vec<f64>>,
    /// The volume associated with each maxima.
    pub bader_volume: Vec<f64>,
    /// The charge associated with each atom. Same form as [`self.bader_charge`]
    /// however inner length is [`Atoms.positions`].len().
    pub atoms_charge: Vec<Vec<f64>>,
    /// The volume associated with each atom.
    pub atoms_volume: Vec<f64>,
    /// The charge (and spin) assigned to the vacuum.
    pub vacuum_charge: Vec<f64>,
    /// The volume assigned to the vacuum.
    pub vacuum_volume: f64,
    /// The total partitioned charge (and spin).
    pub total_charge: Vec<f64>,
}

impl Analysis {
    /// Creates a new instance of Analysis from an associated [`VoxelMap`],
    /// the amount of supplied densities and how many atoms there are.
    ///
    /// * `voxel_map`: The partitioned map of the voxels.
    /// * `densities_len`: How many densities (charge, spin, etc) have been read.
    /// * `atom_num`: The number of atoms in the density file.
    ///
    /// ### Returns:
    /// `Self`: A new [`Analysis`] instance.
    ///
    /// ### Examples
    /// ```
    /// use bader::analysis::Analysis;
    /// use bader::voxel_map::VoxelMap;
    ///
    /// let voxel_map = VoxelMap::new([2, 2, 5],
    ///                               [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 5.0]],
    ///                               [0.0, 0.0, 0.0]);
    /// (0..20).for_each(|p| voxel_map.maxima_store(p, p.rem_euclid(2)));
    /// let analysis = Analysis::new(&voxel_map, 2, 2);
    /// assert_eq!(analysis.bader_maxima, vec![0, 1])
    /// ```
    pub fn new(voxel_map: &VoxelMap,
               densities_len: usize,
               atom_num: usize)
               -> Self {
        let bader_maxima = voxel_map.maxima_list();
        let mut maxima_index = FxHashMap::<usize, usize>::default();
        for (i, maxima) in bader_maxima.iter().enumerate() {
            maxima_index.insert(*maxima, i);
        }
        // I would like to allocate this with error checks.
        let assigned_atom = Vec::with_capacity(0);
        let minimum_distance = Vec::with_capacity(0);
        let surface_distance = Vec::with_capacity(0);
        let bader_charge = vec![Vec::with_capacity(0); densities_len];
        let bader_volume = Vec::with_capacity(0);
        let atoms_charge = vec![vec![0f64; atom_num]; densities_len];
        let atoms_volume = vec![0f64; atom_num];
        let vacuum_charge = vec![0f64; densities_len];
        let vacuum_volume = 0f64;
        let total_charge = vec![0f64; densities_len];
        Self { assigned_atom,
               minimum_distance,
               surface_distance,
               maxima_index,
               bader_maxima,
               bader_charge,
               bader_volume,
               atoms_charge,
               atoms_volume,
               vacuum_charge,
               vacuum_volume,
               total_charge }
    }

    /// Returns the index of a specific maxima in [`self.bader_maxima`].
    fn index_get(&self, maxima: usize) -> Result<usize, AnalysisError> {
        match self.maxima_index.get(&maxima) {
            Some(index) => Ok(*index),
            None => Err(AnalysisError::NotMaxima),
        }
    }

    /// Returns the atoms to which the maxima is assigned.
    fn atom_get(&self, maxima: usize) -> Result<usize, AnalysisError> {
        let i = self.index_get(maxima)?;
        Ok(self.assigned_atom[i])
    }

    /// Checks if point, p, is a boundary between atoms.
    ///
    /// * `p`: The point to be checked.
    /// * `atom_number`: The atom associated to point `p`.
    /// * `grid`: The [`Grid`] associated with the density.
    /// * `voxel_map`: The [`VoxelMap`] in which `p` is in.
    ///
    /// ### Returns
    /// `Result<bool, [`AnalysisError`]>`: True or false wrapped with error in
    /// locating index for maxima.
    pub fn is_atom_boundary(&self,
                            p: isize,
                            atom_number: usize,
                            grid: &Grid,
                            voxel_map: &VoxelMap)
                            -> Result<bool, AnalysisError> {
        for shift in grid.voronoi.vectors.iter() {
            let pn = grid.voronoi_shift(p, shift);
            let maxima = voxel_map.maxima_get(pn);
            match maxima.cmp(&-1) {
                std::cmp::Ordering::Equal => return Ok(true),
                _ => {
                    if self.atom_get(maxima as usize)? != atom_number {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Assigns each Bader maxima to an atom recording the distance between
    /// maxima and atom position.
    pub fn assign_atoms(&mut self, atoms: &Atoms, grid: &Grid, pbar: Bar) {
        let mut assigned_atom = Vec::with_capacity(atoms.positions.len());
        let mut minimum_distance = Vec::with_capacity(atoms.positions.len());
        for maxima in self.bader_maxima.iter() {
            let maxima_cartesian = grid.to_cartesian(*maxima as isize);
            let maxima_cartesian = {
                utils::dot(maxima_cartesian, grid.voxel_lattice.to_cartesian)
            };
            let mut maxima_lll_fractional = utils::dot(maxima_cartesian,
                                                       atoms.reduced_lattice
                                                            .to_fractional);
            for f in &mut maxima_lll_fractional {
                *f = f.rem_euclid(1.);
            }
            let maxima_lll_cartesian = utils::dot(maxima_lll_fractional,
                                                  atoms.reduced_lattice
                                                       .to_cartesian);
            let mut atom_num = 0;
            let mut min_distance = f64::INFINITY;
            for (i, atom) in atoms.reduced_positions.iter().enumerate() {
                for atom_shift in
                    atoms.reduced_lattice.cartesian_shift_matrix.iter()
                {
                    let distance = {
                        (maxima_lll_cartesian[0] - (atom[0] + atom_shift[0]))
                            .powi(2)
                            + (maxima_lll_cartesian[1]
                                - (atom[1] + atom_shift[1]))
                                .powi(2)
                            + (maxima_lll_cartesian[2]
                                - (atom[2] + atom_shift[2]))
                                .powi(2)
                    };
                    if distance < min_distance {
                        min_distance = distance;
                        atom_num = i;
                    }
                }
            }
            assigned_atom.push(atom_num);
            minimum_distance.push(min_distance.powf(0.5));
            pbar.tick()
        }
        self.assigned_atom = assigned_atom;
        self.minimum_distance = minimum_distance;
    }

    /// Sums the densities for each bader volume.
    pub fn charge_sum(&mut self,
                      atoms: &Atoms,
                      densities: &[Vec<f64>],
                      voxel_map: &VoxelMap,
                      pbar: Bar)
                      -> Result<(), AnalysisError> {
        let grid = &voxel_map.grid;
        let mut minimum_distance = vec![f64::INFINITY; atoms.positions.len()];
        let mut bader_charge =
            vec![vec![0.; self.bader_maxima.len()]; self.bader_charge.len()];
        let mut bader_volume = vec![0.; self.bader_maxima.len()];
        let volume = grid.voxel_lattice.volume;
        for p in 0..grid.size.total {
            match voxel_map.voxel_get(p as isize) {
                Voxel::Weight(weights) => {
                    let atom_num = self.atom_get(weights[0] as usize)?;
                    let mut is_atom_boundary = false;
                    for maxima_weight in weights.iter() {
                        let maxima = *maxima_weight as usize;
                        let weight = maxima_weight - maxima as f64;
                        if atom_num != self.atom_get(maxima)? {
                            is_atom_boundary = true;
                        }
                        let i = self.index_get(maxima)?;
                        bader_volume[i] += weight;
                        for (j, charge) in densities.iter().enumerate() {
                            bader_charge[j][i] += weight * charge[p];
                        }
                    }
                    if is_atom_boundary {
                        let p_cartesian = grid.to_cartesian(p as isize);
                        let p_cartesian = utils::dot(p_cartesian,
                                                     grid.voxel_lattice
                                                         .to_cartesian);
                        let mut p_lll_fractional =
                            utils::dot(p_cartesian,
                                       atoms.reduced_lattice.to_fractional);
                        for f in &mut p_lll_fractional {
                            *f = f.rem_euclid(1.);
                        }
                        let p_lll_cartesian = utils::dot(p_lll_fractional,
                                                         atoms.reduced_lattice
                                                              .to_cartesian);
                        let atom = atoms.reduced_positions[atom_num];
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
                            if distance < minimum_distance[atom_num] {
                                minimum_distance[atom_num] = distance;
                            }
                        }
                    }
                }
                Voxel::Maxima(maxima) => {
                    let i = self.index_get(maxima)?;
                    bader_volume[i] += 1.;
                    for (j, charge) in densities.iter().enumerate() {
                        bader_charge[j][i] += charge[p];
                    }
                }
                Voxel::Vacuum => {
                    self.vacuum_volume += volume;
                    for (j, charge) in densities.iter().enumerate() {
                        self.vacuum_charge[j] += volume * charge[p];
                    }
                }
            }
            pbar.tick();
        }
        for (i, charge) in bader_charge.into_iter().enumerate() {
            self.bader_charge[i] = charge.into_iter()
                                         .map(|r| r * grid.voxel_lattice.volume)
                                         .collect();
        }
        self.bader_volume = bader_volume.into_iter()
                                        .map(|r| r * grid.voxel_lattice.volume)
                                        .collect();
        self.surface_distance = minimum_distance.into_iter()
                                                .map(|d| {
                                                    if d == f64::INFINITY {
                                                        0f64
                                                    } else {
                                                        d.powf(0.5)
                                                    }
                                                })
                                                .collect();
        Ok(())
    }

    /// Sums the densities for each atom.
    pub fn atoms_charge_sum(&mut self) {
        for (maxima_i, atom_num) in self.assigned_atom.iter().enumerate() {
            for (i, charge) in self.bader_charge.iter().enumerate() {
                self.atoms_charge[i][*atom_num] += charge[maxima_i];
                self.total_charge[i] += charge[maxima_i];
            }
            self.atoms_volume[*atom_num] += self.bader_volume[maxima_i];
        }
    }

    /// Creates a voxel map for a specific atom.
    pub fn output_atom_map(&self,
                           grid: &Grid,
                           voxel_map: &VoxelMap,
                           atom_num: usize,
                           pbar: Bar)
                           -> Vec<Option<f64>> {
        (0..grid.size.total).map(|p| {
                                let w = match voxel_map.voxel_get(p as isize) {
                                    Voxel::Maxima(maxima) => {
                                        if self.atom_get(maxima).unwrap()
                                           == atom_num
                                        {
                                            Some(1f64)
                                        } else {
                                            None
                                        }
                                    }
                                    Voxel::Weight(weights) => {
                                        let mut w = None;
                                        for maxima_weight in weights {
                                            let maxima =
                                                *maxima_weight as usize;
                                            let weight =
                                                maxima_weight - maxima as f64;
                                            if self.atom_get(maxima).unwrap()
                                               == atom_num
                                            {
                                                w = Some(weight);
                                                break;
                                            }
                                        }
                                        w
                                    }
                                    Voxel::Vacuum => None,
                                };
                                pbar.tick();
                                w
                            })
                            .collect()
    }

    /// Creates a voxel map for a specific volume.
    pub fn output_volume_map(&self,
                             grid: &Grid,
                             voxel_map: &VoxelMap,
                             maxima_out: usize,
                             pbar: Bar)
                             -> Vec<Option<f64>> {
        (0..grid.size.total).map(|p| {
                                let w = match voxel_map.voxel_get(p as isize) {
                                    Voxel::Maxima(maxima) => {
                                        if maxima == maxima_out {
                                            Some(1.)
                                        } else {
                                            None
                                        }
                                    }
                                    Voxel::Weight(weights) => {
                                        let mut w = None;
                                        for maxima_weight in weights {
                                            let maxima =
                                                *maxima_weight as usize;
                                            let weight =
                                                maxima_weight - maxima as f64;
                                            if maxima == maxima_out {
                                                w = Some(weight);
                                                break;
                                            }
                                        }
                                        w
                                    }
                                    Voxel::Vacuum => None,
                                };
                                pbar.tick();
                                w
                            })
                            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_new_all_vacuum() {
        let voxel_map =
            VoxelMap::new([5, 3, 2],
                          [[5.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 2.0]],
                          [0.0, 0.0, 0.0]);
        let analysis = Analysis::new(&voxel_map, 1, 1);
        assert!(analysis.bader_maxima.is_empty())
    }

    #[test]
    fn analysis_new_zero_densities_len() {
        let voxel_map =
            VoxelMap::new([3, 5, 2],
                          [[3.0, 0.0, 0.0], [0.0, 5.0, 0.0], [0.0, 0.0, 2.0]],
                          [0.0, 0.0, 0.0]);
        let analysis = Analysis::new(&voxel_map, 0, 1);
        assert!(analysis.bader_maxima.is_empty())
    }
}

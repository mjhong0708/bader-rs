use anyhow::Result;
use std::fs::File;
use std::io::Write;

/// Create the partitioned charge files using an optional atom map to decide the format
pub fn partitions_file(positions: Vec<(String, String, String)>,
                       partitioned_density: &[Vec<f64>],
                       partitioned_volume: &[f64],
                       total_density: &[f64],
                       total_volume: f64,
                       distance: &[f64],
                       atom_map: Option<&[usize]>)
                       -> Result<String> {
    // calculate the total density for each density supplied
    let total_partitioned_density =
        partitioned_density.iter().fold(vec![
                                            0.0;
                                            partitioned_density[0].len()
                                        ],
                                        |mut sum, d| {
                                            sum.iter_mut()
                                               .zip(d)
                                               .for_each(|(tpd, pd)| {
                                                   *tpd += pd
                                               });
                                            sum
                                        });
    // the volume is the same for all densities
    let total_partitioned_volume = partitioned_volume.iter().sum();
    let vacuum_density = total_partitioned_density.iter()
                                                  .zip(total_density)
                                                  .map(|(a, b)| b - a)
                                                  .collect::<Vec<f64>>();
    let vacuum_volume = total_volume - total_partitioned_volume;
    // which charge file to write, if there's an atom_map -> BCF
    if let Some(atom_map) = atom_map {
        let mut table =
            Table::new(TableType::BaderCharge, partitioned_density[0].len());
        let mut index: Vec<usize> = (0..atom_map.len()).collect();
        index.sort_by(|a, b| atom_map[*a].cmp(&atom_map[*b]));
        let mut atom_num = atom_map[index[0]];
        table.separators.push(atom_num + 1);
        index.into_iter().for_each(|i| {
                             let a = atom_map[i];
                             if a != atom_num {
                                 table.add_separator(a + 1);
                                 atom_num = a
                             }
                             table.add_row(i,
                                           positions[i].clone(),
                                           &partitioned_density[i],
                                           partitioned_volume[i],
                                           distance[i]);
                         });
        Ok(table.get_string(&vacuum_density,
                            vacuum_volume,
                            &total_partitioned_density,
                            total_partitioned_volume))
    // if no atom_map -> ACF
    } else {
        let mut table =
            Table::new(TableType::AtomsCharge, partitioned_density[0].len());
        let mut index = 1;
        positions.into_iter()
                 .zip(partitioned_density)
                 .zip(partitioned_volume)
                 .zip(distance)
                 .for_each(|(((coord, density), volume), distance)| {
                     table.add_row(index, coord, density, *volume, *distance);
                     index += 1;
                 });
        Ok(table.get_string(&vacuum_density,
                            vacuum_volume,
                            &total_partitioned_density,
                            total_partitioned_volume))
    }
}

/// Enum of available tables.
pub enum TableType {
    /// Table for the ACF file.
    AtomsCharge,
    /// Table for the BCF file.
    BaderCharge,
}

/// Structure that contains and builds the table.
struct Table {
    /// How wide each column is.
    column_width: Vec<usize>,
    /// The number of charge and spin densities.
    density_num: usize,
    /// The rows of the table as a vector of strings.
    rows: Vec<Vec<String>>,
    separators: Vec<usize>,
    /// What type of table the structure is.
    table_type: TableType,
}

impl Table {
    /// Creates a new structure and sets the minimum widths of each.
    fn new(table_type: TableType, density_num: usize) -> Self {
        let rows = vec![Vec::with_capacity(0)];
        let mut column_width = Vec::with_capacity(6 + density_num);
        column_width.push(1);
        column_width.push(1);
        column_width.push(1);
        column_width.push(1);
        column_width.push(6);
        match density_num.cmp(&2) {
            std::cmp::Ordering::Greater => {
                column_width.push(6);
                column_width.push(6);
                column_width.push(6);
            }
            std::cmp::Ordering::Equal => column_width.push(6),
            std::cmp::Ordering::Less => (),
        };
        column_width.push(6);
        column_width.push(8);
        let separators = match table_type {
            TableType::AtomsCharge => vec![0, 0],
            TableType::BaderCharge => vec![],
        };
        Self { column_width,
               density_num,
               rows,
               separators,
               table_type }
    }

    /// Adds a row the table.
    fn add_row(&mut self,
               index: usize,
               p: (String, String, String),
               density: &[f64],
               volume: f64,
               distance: f64) {
        let mut row: Vec<String> = Vec::with_capacity(6 + self.density_num);
        row.push(format!("{}", index));
        row.push(p.0);
        row.push(p.1);
        row.push(p.2);
        density.iter().for_each(|d| row.push(format!("{:.6}", d)));
        row.push(format!("{:.6}", volume));
        row.push(format!("{:.6}", distance));
        for (i, col) in row.iter().enumerate() {
            self.column_width[i] = self.column_width[i].max(col.len());
        }
        self.rows.push(row);
    }

    /// Adds a blank row to be a separator in the final string.
    fn add_separator(&mut self, index: usize) {
        self.rows.push(Vec::with_capacity(0));
        self.separators.push(index);
    }

    /// Creates and formats the footer.
    fn format_footer(&self,
                     vacuum_density: &[f64],
                     vacuum_volume: f64,
                     partitioned_density: &[f64],
                     partitioned_volume: f64)
                     -> String {
        match self.table_type {
            TableType::AtomsCharge => {
                let mut separator = self.format_separator(0);
                let footer = match self.density_num.cmp(&2) {
                        std::cmp::Ordering::Less => format!(
                            "\n  Vacuum Charge: {:>18.4}\n  Vacuum Volume: {:>18.4}\n  Partitioned Charge: {:>13.4}\n  Partitioned Volume: {:>13.4}",
                            vacuum_density[0],
                            vacuum_volume,
                            partitioned_density[0],
                            partitioned_volume,
                        ),
                        std::cmp::Ordering::Equal =>  format!(
                            "\n  Vacuum Charge: {:>18.4}\n  Vacuum Spin: {:>20.4}\n  Vacuum Volume: {:>18.4}\n  Partitioned Charge: {:>13.4}\n  Partitioned Spin: {:>15.4}\n  Partitioned Volume: {:>13.4}",
                            vacuum_density[0],
                            vacuum_density[1],
                            vacuum_volume,
                            partitioned_density[0],
                            partitioned_density[1],
                            partitioned_volume,
                        ),
                        std::cmp::Ordering::Greater =>  format!(
                            "\n  Vacuum Charge: {:>18.4}\n  Vacuum Spin X: {:>18.4}\n  Vacuum Spin Y: {:>18.4}\n  Vacuum Spin Z: {:>18.4}\n  Vacuum Volume: {:>18.4}\n  Partitioned Charge: {:>13.4}\n  Partitioned Spin X: {:>13.4}\n  Partitioned Spin Y: {:>13.4}\n  Partitioned Spin Z: {:>13.4}\n  Partitioned Volume: {:>13.4}",
                            vacuum_density[0],
                            vacuum_density[1],
                            vacuum_density[2],
                            vacuum_density[3],
                            vacuum_volume,
                            partitioned_density[0],
                            partitioned_density[1],
                            partitioned_density[2],
                            partitioned_density[3],
                            partitioned_volume,
                        ),
                };
                separator.push_str(&footer);
                separator
            }
            TableType::BaderCharge => String::new(),
        }
    }

    /// Creates and formats the header.
    fn format_header(&self) -> String {
        let mut header = String::new();
        let mut iter = self.column_width.iter();
        header.push_str(&format!(" {:^width$} |",
                                 "#",
                                 width = iter.next().unwrap()));
        header.push_str(&format!(" {:^width$} |",
                                 "X",
                                 width = iter.next().unwrap()));
        header.push_str(&format!(" {:^width$} |",
                                 "Y",
                                 width = iter.next().unwrap()));
        header.push_str(&format!(" {:^width$} |",
                                 "Z",
                                 width = iter.next().unwrap()));
        header.push_str(&format!(" {:^width$} |",
                                 "Charge",
                                 width = iter.next().unwrap()));
        match self.table_type {
            TableType::AtomsCharge | TableType::BaderCharge => {
                match self.density_num.cmp(&2) {
                    std::cmp::Ordering::Equal => {
                        header.push_str(&format!(" {:^width$} |",
                                                 "Spin",
                                                 width = iter.next().unwrap()));
                    }
                    std::cmp::Ordering::Greater => {
                        header.push_str(&format!(" {:^width$} |",
                                                 "Spin X",
                                                 width = iter.next().unwrap()));
                        header.push_str(&format!(" {:^width$} |",
                                                 "Spin Y",
                                                 width = iter.next().unwrap()));
                        header.push_str(&format!(" {:^width$} |",
                                                 "Spin Z",
                                                 width = iter.next().unwrap()));
                    }
                    std::cmp::Ordering::Less => (),
                }
            }
        }
        header.push_str(&format!(" {:^width$} |",
                                 "Volume",
                                 width = iter.next().unwrap()));
        header.push_str(&format!(" {:^width$}\n",
                                 "Distance",
                                 width = iter.next().unwrap()));
        header
    }

    /// Creates and formats a separator.
    fn format_separator(&self, i: usize) -> String {
        let mut separator = String::new();
        self.column_width.iter().for_each(|w| {
            separator.push_str(&format!("-{:-^width$}-+", "-", width = w));
        });
        separator.pop();
        separator.pop();
        if let TableType::BaderCharge = self.table_type {
            let len = self.column_width[0];
            separator.replace_range(1..(len + 7),
                                    &format!("Atom: {:>width$}",
                                             i,
                                             width = len));
        }
        separator
    }

    /// Creates a String representation of the Table.
    fn get_string(self,
                  vacuum_density: &[f64],
                  vacuum_volume: f64,
                  partitioned_density: &[f64],
                  partitioned_volume: f64)
                  -> String {
        let mut separator_iter = self.separators.iter();
        let mut table = String::new();
        table.push_str(&self.format_header());
        self.rows.iter().for_each(|r| {
                            if r.is_empty() {
                                table.push_str(&self.format_separator(*separator_iter.next().unwrap()));
                            } else {
                                let mut row = String::new();
                                r.iter()
                                 .zip(&self.column_width)
                                 .for_each(|(s, w)| {
                                     row.push_str(&format!(" {:>width$} |",
                                                           s,
                                                           width = w))
                                 });
                                row.pop();
                                table.push_str(&row);
                            }
                            table.push('\n');
                        });
        table.push_str(&self.format_footer(vacuum_density,
                                           vacuum_volume,
                                           partitioned_density,
                                           partitioned_volume));
        table
    }
}

/// Write a string to filename. Creates a new file regardless of what exists.
pub fn write(string: String, filename: String) -> std::io::Result<()> {
    let mut bader_file = File::create(&filename)?;
    bader_file.write_all(string.as_bytes())?;
    Ok(())
}

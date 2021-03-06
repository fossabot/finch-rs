#![cfg_attr(feature = "python", feature(specialization))]

extern crate capnp;
#[macro_use]
extern crate serde_derive;

use std::fs::File;
use std::io::{stdin, BufReader, Read};
use std::path::Path;
use std::result::Result as StdResult;

use failure::{format_err, Error};
use memmap::MmapOptions;
use needletail::formats::parse_sequence_reader;
use rayon::prelude::*;

use crate::filtering::FilterParams;
use crate::serialization::{
    read_finch_file, read_mash_file, MultiSketch, Sketch, FINCH_BIN_EXT, FINCH_EXT, MASH_EXT,
};
use crate::sketch_schemes::SketchParams;

pub mod distance;
pub mod filtering;
pub mod sketch_schemes;
// it would be nice if there was a `pub(in main)` or something for
// main_parsing so we don't import it for `lib` itself
pub mod main_parsing;
#[cfg(feature = "python")]
pub mod python;
pub mod serialization;
pub mod statistics;

pub type Result<T> = StdResult<T, Error>;

pub fn sketch_files(
    filenames: &[&str],
    sketch_params: &SketchParams,
    filters: &FilterParams,
) -> Result<Vec<Sketch>> {
    let sketches: Result<Vec<Sketch>> = filenames
        .par_iter()
        .map(|filename| {
            // open the file with a special case to handle stdin
            let sin = stdin();
            let reader: Box<dyn Read> = if filename == &"-" {
                Box::new(sin.lock())
            } else {
                Box::new(File::open(&Path::new(filename))?)
            };
            // sketch!
            Ok(sketch_stream(reader, filename, sketch_params, &filters)?)
        })
        .collect();
    sketches
}

pub fn sketch_stream<'a>(
    reader: Box<dyn Read + 'a>,
    name: &str,
    sketch_params: &SketchParams,
    filters: &FilterParams,
) -> Result<Sketch> {
    let mut filter_params = filters.clone();
    let mut sketcher = sketch_params.create_sketcher();
    parse_sequence_reader(
        reader,
        |seq_type| {
            // disable filtering for FASTA files unless it was explicitly specified
            if filter_params.filter_on.is_none() {
                filter_params.filter_on = match seq_type {
                    "FASTA" => Some(false),
                    "FASTQ" => Some(true),
                    _ => panic!("Unknown sequence type"),
                };
            }
        },
        |seq| {
            sketcher.process(seq);
        },
    )
    .map_err(|e| format_err!("{}", e.to_string()))?;

    let (seq_length, num_valid_kmers) = sketcher.total_bases_and_kmers();
    let hashes = sketcher.to_vec();

    // do filtering
    let mut filtered_hashes = filter_params.filter_counts(&hashes);
    sketch_params.process_post_filter(&mut filtered_hashes, name)?;

    Ok(Sketch {
        name: name.to_string(),
        seq_length,
        num_valid_kmers,
        comment: "".to_string(),
        hashes: filtered_hashes,
        filter_params,
        sketch_params: sketch_params.clone(),
    })
}

pub fn open_sketch_file(filename: &str) -> Result<Vec<Sketch>> {
    let file = File::open(filename).map_err(|_| format_err!("Error opening {}", &filename))?;
    if filename.ends_with(MASH_EXT) {
        let mut buf_reader = BufReader::new(file);
        read_mash_file(&mut buf_reader)
    } else if filename.ends_with(FINCH_BIN_EXT) {
        let mut buf_reader = BufReader::new(file);
        read_finch_file(&mut buf_reader)
    } else if filename.ends_with(FINCH_EXT) || filename.ends_with(".json") {
        let mapped = unsafe { MmapOptions::new().map(&file)? };
        let multisketch: MultiSketch = serde_json::from_slice(&mapped)
            .map_err(|_| format_err!("Error parsing {}", &filename))?;
        multisketch.to_sketches()
    } else {
        Err(format_err!("File suffix is not *.bsk, *.msh, or *.sk"))
    }
}

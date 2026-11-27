//! Extracts the pinned Vanilla data (cached per machine) and generates the tables into
//! `OUT_DIR`; see `vanillite_datagen::build`.

fn main() {
    vanillite_datagen::build::vanilla_data();
}

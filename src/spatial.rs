//! Spatial helpers the harness needs — its OWN copy, deliberately not the server's.
//!
//! Boundary rule (#244): the harness must build and run against any build-5875 server, so it
//! cannot take a path dependency on a particular server's crates. Two of the three things it used
//! to import from the server's shared crate (`lyracore-shared`) are spatial: the vanilla
//! map-coordinate convention (a property of the 1.12 client, identical everywhere) and the
//! interest-grid cell size (a SERVER TUNING value that happens to be 50yd on LyraCore).
//!
//! The split below keeps that honest:
//!   * [`MAP_COORD_MAX`] and [`grid_cell_of`] are protocol/world facts — any 5875 server that grids
//!     its world at all grids it from `+COORD_MAX` downwards.
//!   * [`DEFAULT_GRID_CELL_SIZE`] is only a DEFAULT. A harness scenario that asserts on cell
//!     crossings against a server with a different cell size passes its own size to
//!     [`grid_cell_of`]; nothing here reads server configuration.
//!
//! Sharing the server's implementation would also have been wrong for a test tool even if the
//! dependency were free: a decoder/geometry bug shared by both sides cancels out and the assert
//! goes green on a broken server.

/// Vanilla map coordinate span: cells are measured from `+MAP_COORD_MAX` downwards, the convention
/// the 1.12 client and every mangos-lineage server grid share.
pub const MAP_COORD_MAX: f32 = 17066.666;

/// Default side length, in yards, of one interest-management grid cell. LyraCore's AOI grid uses
/// 50yd cells; a scenario targeting a server with different tuning passes its own value to
/// [`grid_cell_of`] rather than editing this.
pub const DEFAULT_GRID_CELL_SIZE: f32 = 50.0;

/// The grid cell `(x, y)` a world position falls in, for an arbitrary `cell_size`.
/// A non-positive `cell_size` degrades to [`DEFAULT_GRID_CELL_SIZE`] rather than producing
/// infinities — a bad CLI value must not silently make every position land in "cell 0".
pub fn grid_cell_of(x: f32, y: f32, cell_size: f32) -> (i32, i32) {
    let cell = if cell_size > 0.0 {
        cell_size
    } else {
        DEFAULT_GRID_CELL_SIZE
    };
    let gx = ((MAP_COORD_MAX - x) / cell).floor() as i32;
    let gy = ((MAP_COORD_MAX - y) / cell).floor() as i32;
    (gx, gy)
}

/// [`grid_cell_of`] at the default cell size.
pub fn grid_cell(x: f32, y: f32) -> (i32, i32) {
    grid_cell_of(x, y, DEFAULT_GRID_CELL_SIZE)
}

/// Planar (x/y) distance in yards — the range test vanilla itself uses for most gameplay gates.
pub fn distance_2d(a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    (dx * dx + dy * dy).sqrt()
}

/// Full 3D distance in yards.
pub fn distance_3d(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let (dx, dy, dz) = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_indices_increase_as_coordinates_decrease() {
        // The axis is inverted (cells count down from +MAP_COORD_MAX), which is the detail a
        // hand-rolled reimplementation gets backwards.
        let near = grid_cell(0.0, 0.0);
        let far = grid_cell(-1000.0, -1000.0);
        assert!(
            far.0 > near.0 && far.1 > near.1,
            "{far:?} must index above {near:?}"
        );
    }

    #[test]
    fn a_point_and_a_point_one_cell_away_differ_by_exactly_one() {
        let (x, y) = (-8920.0, -180.0);
        let a = grid_cell_of(x, y, 50.0);
        let b = grid_cell_of(x - 50.0, y, 50.0);
        assert_eq!(b.0, a.0 + 1);
        assert_eq!(b.1, a.1);
    }

    #[test]
    fn cell_size_is_a_parameter_not_a_baked_constant() {
        // Same 60yd step: one cell at 50yd tuning, none at 100yd tuning.
        let (x, y) = (-8920.0, -180.0);
        assert_ne!(grid_cell_of(x, y, 50.0), grid_cell_of(x - 60.0, y, 50.0));
        let a = grid_cell_of(x, y, 100.0);
        let b = grid_cell_of(x - 5.0, y, 100.0);
        assert_eq!(a, b, "a 5yd step cannot cross a 100yd cell");
    }

    #[test]
    fn a_degenerate_cell_size_falls_back_instead_of_producing_infinities() {
        assert_eq!(
            grid_cell_of(-8920.0, -180.0, 0.0),
            grid_cell(-8920.0, -180.0)
        );
        assert_eq!(
            grid_cell_of(-8920.0, -180.0, -7.0),
            grid_cell(-8920.0, -180.0)
        );
    }

    #[test]
    fn distances_are_yards() {
        assert_eq!(distance_2d((0.0, 0.0), (3.0, 4.0)), 5.0);
        assert_eq!(distance_3d((0.0, 0.0, 0.0), (0.0, 3.0, 4.0)), 5.0);
        // 2d ignores z; 3d does not.
        assert_eq!(distance_2d((0.0, 0.0), (0.0, 0.0)), 0.0);
        assert_eq!(distance_3d((0.0, 0.0, 0.0), (0.0, 0.0, 9.0)), 9.0);
    }
}

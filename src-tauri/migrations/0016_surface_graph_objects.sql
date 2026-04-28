CREATE TABLE surface_graph_objects (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    surface_id  INTEGER NOT NULL REFERENCES note_surfaces(id) ON DELETE CASCADE,
    mode        TEXT    NOT NULL CHECK(mode IN ('equation', 'points')),
    equation    TEXT,
    points_json TEXT,
    x_min       REAL    NOT NULL,
    x_max       REAL    NOT NULL,
    y_min       REAL    NOT NULL,
    y_max       REAL    NOT NULL,
    bbox_x      REAL    NOT NULL,
    bbox_y      REAL    NOT NULL,
    bbox_w      REAL    NOT NULL,
    bbox_h      REAL    NOT NULL,
    line_colour TEXT    NOT NULL DEFAULT '#2351d1',
    line_width  REAL    NOT NULL DEFAULT 2.0,
    created_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (x_max > x_min),
    CHECK (y_max > y_min),
    CHECK (bbox_w > 0),
    CHECK (bbox_h > 0),
    CHECK (
        (mode = 'equation' AND equation IS NOT NULL AND TRIM(equation) != '' AND points_json IS NULL)
        OR
        (mode = 'points' AND points_json IS NOT NULL AND TRIM(points_json) != '' AND equation IS NULL)
    )
);

CREATE INDEX idx_surface_graph_objects_surface
    ON surface_graph_objects(surface_id);

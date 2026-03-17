use std::path::PathBuf;

use egui::{pos2, Pos2, Rect};
use glam::{Mat3, Mat4, Vec2, Vec3, Vec4};

use crate::domain::{GeoPoint, Workspace};
use crate::map::osm::wrap_tile_x;
use crate::traffic::GeoBounds;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub z: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug)]
pub struct TilePlacement {
    pub key: TileKey,
    pub cache_path: PathBuf,
    pub corners: [Vec3; 4],
}

#[derive(Clone, Debug)]
pub struct MapProjector {
    rect: Rect,
    world_size: f32,
    center_world: Vec2,
    world_wrap_width: f32,
    meters_per_pixel: f32,
    view_proj: Mat4,
    inv_view_proj: Mat4,
    tile_size: f32,
    tile_zoom: u32,
    tile_zoom_scale: f32,
}

impl MapProjector {
    pub fn new(rect: Rect, workspace: &Workspace, tile_size: f32) -> Self {
        let camera = &workspace.app_state.camera;
        let zoom = camera.zoom.clamp(1.0, 18.0);
        let world_size = tile_size * 2.0_f32.powf(zoom);
        let center_world = lat_lon_to_world(camera.center, world_size);
        let meters_per_pixel = mercator_meters_per_pixel(camera.center.lat as f32, zoom).max(0.1);
        let aspect = (rect.width() / rect.height().max(1.0)).max(0.1);
        let fov_y = 45.0_f32.to_radians();
        let focal_length = rect.height() * 0.5 / (fov_y * 0.5).tan();
        let radius = focal_length * 1.35;
        let tilt = camera.tilt_degrees.to_radians().clamp(0.0, 1.45);
        let yaw = -camera.bearing_degrees.to_radians();

        let eye_local = Vec3::new(
            0.0,
            radius * tilt.cos().max(0.05),
            radius * tilt.sin() + 0.001,
        );
        let eye = Mat3::from_rotation_y(yaw) * eye_local;
        let forward = (-eye).normalize();
        let up_hint = if forward.dot(Vec3::Y).abs() > 0.98 {
            Mat3::from_rotation_y(yaw) * Vec3::Z
        } else {
            Vec3::Y
        };
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, up_hint);
        let proj = Mat4::perspective_rh(fov_y, aspect, 0.1, world_size.max(4096.0) * 8.0);
        let view_proj = proj * view;
        let inv_view_proj = view_proj.inverse();

        let tile_zoom = zoom.floor() as u32;
        let tile_zoom_scale = 2.0_f32.powf(zoom - tile_zoom as f32);

        Self {
            rect,
            world_size,
            center_world,
            world_wrap_width: world_size,
            meters_per_pixel,
            view_proj,
            inv_view_proj,
            tile_size,
            tile_zoom,
            tile_zoom_scale,
        }
    }

    pub fn geo_to_world(&self, point: GeoPoint) -> Vec3 {
        let mut delta = lat_lon_to_world(point, self.world_size) - self.center_world;
        if delta.x.abs() > self.world_wrap_width * 0.5 {
            delta.x -= self.world_wrap_width.copysign(delta.x);
        }

        Vec3::new(
            delta.x,
            point.altitude_m.unwrap_or(0.0) / self.meters_per_pixel,
            delta.y,
        )
    }

    pub fn world_to_geo(&self, point: Vec3) -> GeoPoint {
        let world = self.center_world + Vec2::new(point.x, point.z);
        let mut geo = world_to_lat_lon(world, self.world_size);
        geo.altitude_m = Some(point.y * self.meters_per_pixel);
        geo
    }

    pub fn geo_to_screen(&self, point: GeoPoint) -> Option<Pos2> {
        self.world_to_screen(self.geo_to_world(point))
    }

    pub fn world_to_screen(&self, point: Vec3) -> Option<Pos2> {
        let clip = self.view_proj * Vec4::new(point.x, point.y, point.z, 1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        if !ndc.is_finite() {
            return None;
        }
        Some(pos2(
            self.rect.left() + (ndc.x + 1.0) * 0.5 * self.rect.width(),
            self.rect.top() + (1.0 - (ndc.y + 1.0) * 0.5) * self.rect.height(),
        ))
    }

    pub fn screen_to_geo(&self, screen_position: Pos2) -> GeoPoint {
        let x = ((screen_position.x - self.rect.left()) / self.rect.width()) * 2.0 - 1.0;
        let y = 1.0 - ((screen_position.y - self.rect.top()) / self.rect.height()) * 2.0;
        let near = self.inv_view_proj * Vec4::new(x, y, 0.0, 1.0);
        let far = self.inv_view_proj * Vec4::new(x, y, 1.0, 1.0);
        let near = near.truncate() / near.w.max(0.0001);
        let far = far.truncate() / far.w.max(0.0001);
        let direction = (far - near).normalize_or_zero();
        let t = if direction.y.abs() <= 0.0001 {
            0.0
        } else {
            -near.y / direction.y
        };
        self.world_to_geo(near + direction * t.max(0.0))
    }

    pub fn visible_tiles(
        &self,
        workspace: &Workspace,
        cache_root: &std::path::Path,
    ) -> Vec<TilePlacement> {
        let camera = &workspace.app_state.camera;
        let base_world_size = self.tile_size * (1_u32 << self.tile_zoom) as f32;
        let center_world = lat_lon_to_world(camera.center, base_world_size) * self.tile_zoom_scale;
        let tiles_per_side = 1_i32 << self.tile_zoom;
        let tile_world_size = self.tile_size * self.tile_zoom_scale;
        let tile_radius_x = (self.rect.width() / tile_world_size).ceil() as i32 + 3;
        let tile_radius_y = (self.rect.height() / tile_world_size).ceil() as i32 + 3;
        let center_tile_x = (center_world.x / tile_world_size).floor() as i32;
        let center_tile_y = (center_world.y / tile_world_size).floor() as i32;
        let scene_wrap_width = base_world_size * self.tile_zoom_scale;

        let mut tiles = Vec::new();
        for x in (center_tile_x - tile_radius_x)..=(center_tile_x + tile_radius_x) {
            for y in (center_tile_y - tile_radius_y)..=(center_tile_y + tile_radius_y) {
                if y < 0 || y >= tiles_per_side {
                    continue;
                }

                let wrapped_x = wrap_tile_x(x, self.tile_zoom);
                let left = x as f32 * tile_world_size;
                let top = y as f32 * tile_world_size;
                let right = left + tile_world_size;
                let bottom = top + tile_world_size;
                let corners_2d = [
                    Vec2::new(left, top),
                    Vec2::new(right, top),
                    Vec2::new(right, bottom),
                    Vec2::new(left, bottom),
                ];
                let corners = corners_2d.map(|corner| {
                    let mut delta = corner - center_world;
                    if delta.x.abs() > scene_wrap_width * 0.5 {
                        delta.x -= scene_wrap_width.copysign(delta.x);
                    }
                    Vec3::new(delta.x, 0.0, delta.y)
                });

                let cache_path = cache_root
                    .join(self.tile_zoom.to_string())
                    .join(wrapped_x.to_string())
                    .join(format!("{y}.png"));
                tiles.push(TilePlacement {
                    key: TileKey {
                        z: self.tile_zoom,
                        x: wrapped_x,
                        y,
                    },
                    cache_path,
                    corners,
                });
            }
        }
        tiles
    }

    pub fn view_proj(&self) -> Mat4 {
        self.view_proj
    }

    pub fn visible_geo_bounds(&self) -> GeoBounds {
        let sample_points = [
            pos2(self.rect.left(), self.rect.top()),
            pos2(self.rect.right(), self.rect.top()),
            pos2(self.rect.right(), self.rect.bottom()),
            pos2(self.rect.left(), self.rect.bottom()),
            self.rect.center(),
        ];
        let mut lamin = f64::INFINITY;
        let mut lomin = f64::INFINITY;
        let mut lamax = f64::NEG_INFINITY;
        let mut lomax = f64::NEG_INFINITY;
        for point in sample_points {
            let geo = self.screen_to_geo(point);
            lamin = lamin.min(geo.lat);
            lomin = lomin.min(geo.lon);
            lamax = lamax.max(geo.lat);
            lomax = lomax.max(geo.lon);
        }
        GeoBounds {
            lamin,
            lomin,
            lamax,
            lomax,
        }
        .normalized()
    }
}

fn lat_lon_to_world(point: GeoPoint, world_size: f32) -> Vec2 {
    let x = ((point.lon as f32 + 180.0) / 360.0) * world_size;
    let lat_rad = point.lat.to_radians() as f32;
    let mercator = (std::f32::consts::PI / 4.0 + lat_rad / 2.0).tan().ln();
    let y = (1.0 - mercator / std::f32::consts::PI) / 2.0 * world_size;
    Vec2::new(x, y.clamp(0.0, world_size))
}

fn world_to_lat_lon(world: Vec2, world_size: f32) -> GeoPoint {
    let lon = world.x / world_size * 360.0 - 180.0;
    let mercator_y = std::f32::consts::PI * (1.0 - 2.0 * (world.y / world_size));
    let lat = (2.0 * mercator_y.exp().atan() - std::f32::consts::PI / 2.0).to_degrees();
    GeoPoint {
        lat: lat as f64,
        lon: lon as f64,
        altitude_m: Some(0.0),
    }
}

fn mercator_meters_per_pixel(latitude_deg: f32, zoom: f32) -> f32 {
    let latitude = latitude_deg.to_radians().cos().abs().max(0.05);
    156_543.03392 * latitude / 2.0_f32.powf(zoom)
}

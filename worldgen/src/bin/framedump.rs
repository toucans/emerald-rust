//! Debug aid: render one frame of the real world with a tiny software
//! rasterizer (both passes: offscreen target, then screen composite) and
//! write it to a PNG, so extraction + baking + the post pass can be
//! eyeballed without a display. Usage:
//!   cargo run -p worldgen --bin framedump -- [out.png] [steps-right] [zoom-steps]

use types::{Flip, Frame, Quad, TextureId};

struct Tex {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

fn blit(fb: &mut Tex, quads: &[Quad], atlases: &[Tex], target: Option<&Tex>) {
    let mut sorted = quads.to_vec();
    sorted.sort_by_key(|q| q.layer);
    for q in &sorted {
        let tex = if q.tex == TextureId::TARGET {
            match target {
                Some(t) => t,
                None => continue,
            }
        } else {
            &atlases[q.tex.0 as usize]
        };
        for py in 0..q.dst.h.round() as i32 {
            for px in 0..q.dst.w.round() as i32 {
                let dx = q.dst.x as i32 + px;
                let dy = q.dst.y as i32 + py;
                if dx < 0 || dy < 0 || dx >= fb.w as i32 || dy >= fb.h as i32 {
                    continue;
                }
                let mut u = px as f32 / q.dst.w * q.src.w;
                let mut v = py as f32 / q.dst.h * q.src.h;
                if matches!(q.flip, Flip::H | Flip::Both) {
                    u = q.src.w - 1.0 - u;
                }
                if matches!(q.flip, Flip::V | Flip::Both) {
                    v = q.src.h - 1.0 - v;
                }
                let sx = ((q.src.x + u) as u32).min(tex.w - 1);
                let sy = ((q.src.y + v) as u32).min(tex.h - 1);
                let s = ((sy * tex.w + sx) * 4) as usize;
                let a = tex.rgba[s + 3] as u32 * q.tint.a as u32 / 255;
                if a == 0 {
                    continue;
                }
                let d = ((dy as u32 * fb.w + dx as u32) * 4) as usize;
                let tint = [q.tint.r, q.tint.g, q.tint.b];
                for c in 0..3 {
                    let src = tex.rgba[s + c] as u32 * tint[c] as u32 / 255;
                    let dst = fb.rgba[d + c] as u32;
                    fb.rgba[d + c] = (src * a / 255 + dst * (255 - a) / 255) as u8;
                }
                fb.rgba[d + 3] = 255;
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "frame.png".into());
    let steps: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let zoom_steps: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let pack = std::fs::read("assets/world.bin").expect("run worldgen first");
    let mut atlases: Vec<Tex> = Vec::new();
    let mut world = game::World::new(&pack, &mut |rgba, w, h| {
        atlases.push(Tex { rgba: rgba.to_vec(), w, h });
        TextureId(atlases.len() as u32 - 1)
    })
    .unwrap();

    for _ in 0..steps {
        world.step(types::Input { right: true, ..Default::default() });
    }
    for _ in 0..zoom_steps.unsigned_abs() {
        world.step(types::Input {
            zoom_in: zoom_steps > 0,
            zoom_out: zoom_steps < 0,
            ..Default::default()
        });
    }
    match std::env::args().nth(4).as_deref() {
        Some("menu") => {
            world.step(types::Input { start: true, ..Default::default() });
            world.step(types::Input::default());
        }
        Some("surf") | Some("bike") => {
            // teleport onto/next to Route 115's sea (west edge) via a save
            let mode = std::env::args().nth(4).unwrap();
            let mut st = *world.state();
            st.player.x = if mode == "surf" { 2 } else { 8 };
            st.player.y = if mode == "surf" { 62 } else { 58 };
            st.player.elevation = if mode == "surf" { 1 } else { 3 };
            st.player.avatar =
                if mode == "surf" { game::player::Avatar::Surfing } else { game::player::Avatar::Bike { tier: 0, momentum: 0 } };
            st.player.facing = game::player::Facing::Left;
            st.cam.x = st.player.x as f32 * 16.0 + 8.0;
            st.cam.y = st.player.y as f32 * 16.0 + 8.0;
            world
                .load_state(&st.to_bytes())
                .expect("teleport save state");
        }
        Some("door") => {
            // find the first animated door with a warp in the overworld,
            // stand below it, walk up into it, snapshot mid-open
            let (mut dx, mut dy) = (0, 0);
            'search: for y in 0..world.graph().realm(0).height {
                for x in 0..world.graph().realm(0).width {
                    if world.graph().door_at(0, x, y).is_some()
                        && world.graph().realm(0).warp_at(x, y).is_some()
                    {
                        (dx, dy) = (x, y);
                        break 'search;
                    }
                }
            }
            println!("door at ({dx}, {dy})");
            let mut st = *world.state();
            st.player.x = dx;
            st.player.y = dy + 1;
            st.player.facing = game::player::Facing::Up;
            st.cam.x = dx as f32 * 16.0 + 8.0;
            st.cam.y = (dy + 1) as f32 * 16.0 + 8.0;
            world.load_state(&st.to_bytes()).expect("teleport");
            for _ in 0..12 {
                world.step(types::Input { up: true, ..Default::default() });
            }
        }
        _ => {}
    }

    let (sw, sh) = (480u32, 320u32);
    let mut frame = Frame::default();
    world.frame(1.0, sw as f32, sh as f32, &mut frame);

    let (tw, th) = frame.target_size;
    let mut target = Tex { rgba: vec![0; (tw * th * 4) as usize], w: tw, h: th };
    blit(&mut target, &frame.offscreen, &atlases, None);

    let mut fb = Tex { rgba: vec![0; (sw * sh * 4) as usize], w: sw, h: sh };
    blit(&mut fb, &frame.screen, &atlases, Some(&target));

    let f = std::fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), sw, sh);
    enc.set_color(png::ColorType::Rgba);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(&fb.rgba).unwrap();
    println!(
        "wrote {out} ({}+{} quads, {}x{} target)",
        frame.offscreen.len(),
        frame.screen.len(),
        tw,
        th
    );
}

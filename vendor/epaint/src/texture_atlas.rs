use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// MCE 诊断:整图因仍被持有而发生的深拷贝次数(应为 0 或极少)。
#[doc(hidden)]
pub static MCE_IMAGE_CLONES: AtomicU64 = AtomicU64::new(0);
/// MCE 诊断:部分增量上传缓冲的新分配次数。
#[doc(hidden)]
pub static MCE_UPLOAD_ALLOCS: AtomicU64 = AtomicU64::new(0);
/// MCE 诊断:图集实例创建次数(重建 = 从 32 高重新增长)。
#[doc(hidden)]
pub static MCE_ATLAS_CREATES: AtomicU64 = AtomicU64::new(0);
/// MCE 诊断:图集高度倍增次数(每次倍增 = 一次大块重分配)。
#[doc(hidden)]
pub static MCE_ATLAS_GROWTHS: AtomicU64 = AtomicU64::new(0);
/// MCE 诊断:Fonts 因 TextOptions 变化而重建整张字形图集的次数。
#[doc(hidden)]
pub static MCE_SET_OPTIONS: AtomicU64 = AtomicU64::new(0);

use ecolor::Color32;
use emath::{Rect, remap_clamp};

use crate::{ColorImage, ImageDelta, TextOptions};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rectu {
    /// inclusive
    min_x: usize,

    /// inclusive
    min_y: usize,

    /// exclusive
    max_x: usize,

    /// exclusive
    max_y: usize,
}

impl Rectu {
    const NOTHING: Self = Self {
        min_x: usize::MAX,
        min_y: usize::MAX,
        max_x: 0,
        max_y: 0,
    };
    const EVERYTHING: Self = Self {
        min_x: 0,
        min_y: 0,
        max_x: usize::MAX,
        max_y: usize::MAX,
    };
}

#[derive(Copy, Clone, Debug)]
struct PrerasterizedDisc {
    r: f32,
    uv: Rectu,
}

/// A pre-rasterized disc (filled circle), somewhere in the texture atlas.
#[derive(Copy, Clone, Debug)]
pub struct PreparedDisc {
    /// The radius of this disc in texels.
    pub r: f32,

    /// Width in texels.
    pub w: f32,

    /// Where in the texture atlas the disc is.
    /// Normalized in 0-1 range.
    pub uv: Rect,
}

/// Contains font data in an atlas, where each character occupied a small rectangle.
///
/// More characters can be added, possibly expanding the texture.
#[derive(Clone)]
pub struct TextureAtlas {
    /// MCE: 以 `Arc` 持有,`take_delta` 的整图分支可直接共享同一块内存
    /// (此前每次 clone 深拷贝整张图集)。写路径经 `Arc::make_mut`:
    /// 若上一帧的整图增量仍被持有,才退化为一次深拷贝(正确性不变)。
    image: Arc<ColorImage>,

    /// MCE: 部分增量的**持久上传缓冲池**。按量化后的 2 的幂尺寸缓存,
    /// 同尺寸帧原地复用(逐行 `copy_from_slice`),不再每帧新建 `ColorImage`。
    /// 只缓存未被下游持有(strong_count == 1)的缓冲;总字节数有上限。
    upload: Vec<Arc<ColorImage>>,
    upload_allocs: u64,

    /// What part of the image that is dirty
    dirty: Rectu,

    /// Used for when allocating new rectangles.
    cursor: (usize, usize),

    row_height: usize,

    /// Set when someone requested more space than was available.
    overflowed: bool,

    /// pre-rasterized discs of radii `2^i`, where `i` is the index.
    discs: Vec<PrerasterizedDisc>,

    /// Controls how to convert glyph coverage to alpha.
    options: TextOptions,
}

impl TextureAtlas {
    pub fn new(size: [usize; 2], options: TextOptions) -> Self {
        assert!(size[0] >= 1024, "Tiny texture atlas");
        MCE_ATLAS_CREATES.fetch_add(1, Ordering::Relaxed);
        let mut atlas = Self {
            image: Arc::new(ColorImage::filled(size, Color32::TRANSPARENT)),
            upload: Vec::new(),
            upload_allocs: 0,
            dirty: Rectu::EVERYTHING,
            cursor: (0, 0),
            row_height: 0,
            overflowed: false,
            discs: vec![], // will be filled in below
            options,
        };
        atlas.add_initial_glyphs();
        atlas
    }

    /// MCE: 原地清空图集。
    ///
    /// 上游 `GlyphAtlas::clear` 直接丢弃整个 `TextureAtlas` 再按 32 高重建,
    /// 每轮 clear 都要重新走 32→64→…→4096 的倍增链,产生 4/8/16/32MB 的
    /// 临时分配(macOS 把释放的段留成 `Malloc Large (empty)`,长滚动下
    /// 每轮各留一块)。这里保留尺寸与像素缓冲(Vec 容量),只清内容并重放
    /// 白像素/圆盘初始化,下一轮增长不再分配。
    pub fn clear(&mut self) {
        if Arc::strong_count(&self.image) > 1 {
            MCE_IMAGE_CLONES.fetch_add(1, Ordering::Relaxed);
        }
        let pixels = &mut Arc::make_mut(&mut self.image).pixels;
        pixels.fill(Color32::TRANSPARENT);
        self.cursor = (0, 0);
        self.row_height = 0;
        self.overflowed = false;
        self.dirty = Rectu::EVERYTHING;
        self.discs.clear();
        // 上传缓冲保留:尺寸与图集不变,清空内容不影响后续整块覆盖。
        self.add_initial_glyphs();
    }

    /// 白像素(纯色绘制用)+ 一组预栅格化圆盘;`new` 与 `clear` 共用。
    fn add_initial_glyphs(&mut self) {
        // Make the top left pixel fully white for `WHITE_UV`, i.e. painting something with solid color:
        let (pos, image) = self.allocate((1, 1));
        assert_eq!(
            pos,
            (0, 0),
            "Expected the first allocation to be at (0, 0), but was at {pos:?}"
        );
        image[pos] = Color32::WHITE;

        // Allocate a series of anti-aliased discs used to render small filled circles:
        // TODO(emilk): these circles can be packed A LOT better.
        // In fact, the whole texture atlas could be packed a lot better.
        // for r in [1, 2, 4, 8, 16, 32, 64] {
        //     let w = 2 * r + 3;
        //     let hw = w as i32 / 2;
        const LARGEST_CIRCLE_RADIUS: f32 = 8.0; // keep small so that the initial texture atlas is small
        // 先取出(Copy),避免与 allocate 返回的 &mut image 借用冲突。
        let transfer = self.options.color_transfer_function;
        for i in 0.. {
            let r = 2.0_f32.powf(i as f32 * 0.5 - 1.0);
            if r > LARGEST_CIRCLE_RADIUS {
                break;
            }
            let hw = (r + 0.5).ceil() as i32;
            let w = (2 * hw + 1) as usize;
            let ((x, y), image) = self.allocate((w, w));
            for dx in -hw..=hw {
                for dy in -hw..=hw {
                    let distance_to_center = ((dx * dx + dy * dy) as f32).sqrt();
                    let coverage =
                        remap_clamp(distance_to_center, (r - 0.5)..=(r + 0.5), 1.0..=0.0);
                    image[((x as i32 + hw + dx) as usize, (y as i32 + hw + dy) as usize)] =
                        transfer.color_from_coverage(coverage);
                }
            }
            self.discs.push(PrerasterizedDisc {
                r,
                uv: Rectu {
                    min_x: x,
                    min_y: y,
                    max_x: x + w,
                    max_y: y + w,
                },
            });
        }
    }

    pub fn options(&self) -> &TextOptions {
        &self.options
    }

    pub fn size(&self) -> [usize; 2] {
        self.image.size
    }

    /// Returns the locations and sizes of pre-rasterized discs (filled circles) in this atlas.
    pub fn prepared_discs(&self) -> Vec<PreparedDisc> {
        let size = self.size();
        let inv_w = 1.0 / size[0] as f32;
        let inv_h = 1.0 / size[1] as f32;
        self.discs
            .iter()
            .map(|disc| {
                let r = disc.r;
                let Rectu {
                    min_x,
                    min_y,
                    max_x,
                    max_y,
                } = disc.uv;
                let w = max_x - min_x;
                let uv = Rect::from_min_max(
                    emath::pos2(min_x as f32 * inv_w, min_y as f32 * inv_h),
                    emath::pos2(max_x as f32 * inv_w, max_y as f32 * inv_h),
                );
                PreparedDisc { r, w: w as f32, uv }
            })
            .collect()
    }

    fn max_height(&self) -> usize {
        // the initial width is set to the max size
        self.image.height().max(self.image.width())
    }

    /// When this get high, it might be time to clear and start over!
    pub fn fill_ratio(&self) -> f32 {
        if self.overflowed {
            1.0
        } else {
            (self.cursor.1 + self.row_height) as f32 / self.max_height() as f32
        }
    }

    /// The texture options suitable for a font texture
    #[inline]
    pub fn texture_options() -> crate::textures::TextureOptions {
        crate::textures::TextureOptions::LINEAR
    }

    /// The full font atlas image.
    #[inline]
    pub fn image(&self) -> &ColorImage {
        &self.image
    }

    /// MCE: 图集图像的共享句柄(诊断/测试用;持有期间分配字形可能触发一次深拷贝)。
    #[doc(hidden)]
    #[inline]
    pub fn image_shared(&self) -> Arc<ColorImage> {
        self.image.clone()
    }

    /// MCE: 部分上传缓冲的新分配次数(诊断/测试用;复用成功时不再增长)。
    #[doc(hidden)]
    #[inline]
    pub fn upload_allocations(&self) -> u64 {
        self.upload_allocs
    }

    /// Call to get the change to the image since last call.
    pub fn take_delta(&mut self) -> Option<ImageDelta> {
        let texture_options = Self::texture_options();

        let dirty = core::mem::replace(&mut self.dirty, Rectu::NOTHING);
        if dirty == Rectu::NOTHING {
            None
        } else if dirty == Rectu::EVERYTHING {
            // 整图增量直接共享图集内存(Arc clone,零拷贝)。此前每次
            // `self.image.clone()` 深拷贝整图;clear/扩容时每轮各一次
            // (最大 64MB),滚动长跑下是 Malloc Large 滞留的主项之一。
            Some(ImageDelta::full(self.image.clone(), texture_options))
        } else {
            // ---- MCE 本地补丁(内存修复,勿随上游覆盖)----
            // 每帧的部分增量原本按“本帧脏区”任意尺寸分配一块 ColorImage
            // (4–10MB 级);macOS malloc 按尺寸类缓存释放后的 VM 段
            // (Malloc Large (empty)),滚动时高度逐帧不同 → 尺寸类无界 →
            // 缓存段持续累积(实测滚动峰值 1.9–2.8GB,栈落在本函数)。
            // 修复分两层:
            // 1) 把增量区域**量化到 2 的幂尺寸**(`pos` 必要时回挪以保证覆盖
            //    脏区;多传的行/列是图集里的已有正确像素,只多传、不改内容),
            //    尺寸类压回 ≤13 个;
            // 2) 量化后仍每帧新建 ColorImage。现在改用**持久上传缓冲**:
            //    同尺寸帧原地逐行拷贝,交给下游的是同一块内存的 Arc 共享,
            //    不再产生每帧分配(上一帧增量仍被持有时退回新建,保证正确)。
            let img_w = self.image.width();
            let img_h = self.image.height();
            let mut pos = [dirty.min_x, dirty.min_y];
            let mut size = [dirty.max_x - dirty.min_x, dirty.max_y - dirty.min_y];
            debug_assert!(size[0] > 0 && size[1] > 0);
            let qw = size[0].next_power_of_two().min(img_w);
            let qh = size[1].next_power_of_two().min(img_h);
            if qw > size[0] && pos[0] + qw > img_w {
                pos[0] = img_w - qw;
            }
            if qh > size[1] && pos[1] + qh > img_h {
                pos[1] = img_h - qh;
            }
            size = [qw, qh];

            // 复用:同尺寸且未被下游持有的缓冲(上一帧的 delta 通常已 drop)。
            let mut buf = match self
                .upload
                .iter()
                .position(|b| b.size == size && Arc::strong_count(b) == 1)
            {
                Some(i) => self.upload.swap_remove(i),
                None => {
                    self.upload_allocs += 1;
                    MCE_UPLOAD_ALLOCS.fetch_add(1, Ordering::Relaxed);
                    Arc::new(ColorImage::filled(size, Color32::TRANSPARENT))
                }
            };

            // 逐行把图集脏区(含量化的多传行列)拷进持久缓冲。
            {
                let dst = Arc::get_mut(&mut buf).expect("唯一持有者,刚检查");
                let src = self.image.as_ref();
                for row in 0..size[1] {
                    let s = (pos[1] + row) * img_w + pos[0];
                    let d = row * size[0];
                    dst.pixels[d..d + size[0]]
                        .copy_from_slice(&src.pixels[s..s + size[0]]);
                }
            }
            self.upload.push(buf.clone());
            self.trim_upload_pool();
            Some(ImageDelta::partial(pos, buf, texture_options))
        }
    }

    /// 上传缓冲池上限:最多 8 块 / 64MB。只淘汰未被下游持有的旧缓冲,
    /// 保证缓存不会随图集尺寸或运行时长无界增长。
    fn trim_upload_pool(&mut self) {
        const MAX_BUFFERS: usize = 8;
        const MAX_BYTES: usize = 64 * 1024 * 1024;
        loop {
            let bytes: usize = self
                .upload
                .iter()
                .map(|b| b.size[0] * b.size[1] * 4)
                .sum();
            if self.upload.len() <= MAX_BUFFERS && bytes <= MAX_BYTES {
                break;
            }
            // 末尾是刚放回、下一帧要用的缓冲;从其余空闲者中淘汰最旧的。
            let last = self.upload.len().saturating_sub(1);
            match (0..last).find(|&i| Arc::strong_count(&self.upload[i]) == 1) {
                Some(i) => drop(self.upload.remove(i)),
                None => break,
            }
        }
    }

    /// Returns the coordinates of where the rect ended up,
    /// and invalidates the region.
    pub fn allocate(&mut self, (w, h): (usize, usize)) -> ((usize, usize), &mut ColorImage) {
        /// On some low-precision GPUs (my old iPad) characters get muddled up
        /// if we don't add some empty pixels between the characters.
        /// On modern high-precision GPUs this is not needed.
        const PADDING: usize = 1;

        assert!(
            w <= self.image.width(),
            "Tried to allocate a {} wide glyph in a {} wide texture atlas",
            w,
            self.image.width()
        );
        if self.cursor.0 + w > self.image.width() {
            // New row:
            self.cursor.0 = 0;
            self.cursor.1 += self.row_height + PADDING;
            self.row_height = 0;
        }

        self.row_height = self.row_height.max(h);

        let required_height = self.cursor.1 + self.row_height;

        if required_height > self.max_height() {
            // This is a bad place to be - we need to start reusing space :/

            log::warn!("epaint texture atlas overflowed!");

            self.cursor = (0, self.image.height() / 3); // Restart a bit down - the top of the atlas has too many important things in it
            self.overflowed = true; // this will signal the user that we need to recreate the texture atlas next frame.
        } else if required_height >= self.image.height() && {
            if Arc::strong_count(&self.image) > 1 {
                MCE_IMAGE_CLONES.fetch_add(1, Ordering::Relaxed);
            }
            resize_to_min_height(Arc::make_mut(&mut self.image), required_height)
        } {
            self.dirty = Rectu::EVERYTHING;
        }

        let pos = self.cursor;
        self.cursor.0 += w + PADDING;

        self.dirty.min_x = self.dirty.min_x.min(pos.0);
        self.dirty.min_y = self.dirty.min_y.min(pos.1);
        self.dirty.max_x = self.dirty.max_x.max(pos.0 + w);
        self.dirty.max_y = self.dirty.max_y.max(pos.1 + h);

        if Arc::strong_count(&self.image) > 1 {
            MCE_IMAGE_CLONES.fetch_add(1, Ordering::Relaxed);
        }
        (pos, Arc::make_mut(&mut self.image))
    }
}

fn resize_to_min_height(image: &mut ColorImage, required_height: usize) -> bool {
    while required_height >= image.height() {
        image.size[1] *= 2; // double the height
        MCE_ATLAS_GROWTHS.fetch_add(1, Ordering::Relaxed);
    }

    if image.width() * image.height() > image.pixels.len() {
        image
            .pixels
            .resize(image.width() * image.height(), Color32::TRANSPARENT);
        true
    } else {
        false
    }
}

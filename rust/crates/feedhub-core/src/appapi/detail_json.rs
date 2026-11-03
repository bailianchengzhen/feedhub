//! `/detail` 输出层的改写（去地址、补视频封面、算 segments）与评论图片清单抽取。

use crate::appapi::{
    image_url_of, ApiImage, ContentImages, DetailImages, VIDEO_COVER_NOTE,
};
use crate::hosted::is_kotlin_blank;
use crate::sync::json::{self, Value};

/// 改写 `/detail` 的 JSON，并返回提取出的图片清单 `DetailImages`。
///
/// 改写规则（保持与 Kotlin `detailJson` 逐字节一致）：
/// 1. 顶层追加 `"images": <总张数>`；
/// 2. `item.media` 里的图片剥掉 `url`，视频剥掉 `cover`；
/// 3. `body.blocks` 里的 `img` 块剥掉 `url` 和 `original`，替换为 `idx`、`isCover`、`segments`（若 > 1）、`note`；
/// 4. 视频帖在 `body.blocks` 最前面补一个第 0 张图的 `img` 封面块；
/// 5. `card` 和 `video` 块剥掉 `cover`。
pub fn rewrite_detail_json(detail_json: &str, original_images: bool) -> Result<(String, DetailImages), String> {
    let mut root = match json::parse(detail_json) {
        Ok(Value::Object(fields)) => fields,
        _ => return Err("Invalid detail JSON".to_string()),
    };

    // 1. 提取 media 与 body 中的图片信息
    let mut body_images: Vec<Vec<(String, Value)>> = Vec::new();
    let mut body_urls: Vec<String> = Vec::new();

    if let Some(body) = get_obj(&root, "body") {
        if let Some(blocks) = get_arr(body, "blocks") {
            for b in blocks {
                if let Value::Object(block_fields) = b {
                    if get_str(block_fields, "type") == Some("img") {
                        body_images.push(block_fields.clone());
                        if let Some(u) = get_str(block_fields, "url") {
                            body_urls.push(u.to_string());
                        }
                        if let Some(orig) = get_str(block_fields, "original") {
                            body_urls.push(orig.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut video_cover: Option<(String, Option<i32>, Option<i32>)> = None;
    let mut first_media_image: Option<(String, Option<i32>, Option<i32>)> = None;

    if let Some(item) = get_obj(&root, "item") {
        if let Some(media_list) = get_arr(item, "media") {
            for m in media_list {
                if let Value::Object(m_fields) = m {
                    let m_type = get_str(m_fields, "type");
                    if m_type == Some("video") && video_cover.is_none() {
                        if let Some(c) = get_str(m_fields, "cover") {
                            if !is_kotlin_blank(c) {
                                let w = get_i32(m_fields, "width");
                                let h = get_i32(m_fields, "height");
                                video_cover = Some((c.to_string(), w, h));
                            }
                        }
                    } else if m_type == Some("image") && first_media_image.is_none() {
                        if let Some(u) = get_str(m_fields, "url") {
                            if !is_kotlin_blank(u) {
                                let w = get_i32(m_fields, "width");
                                let h = get_i32(m_fields, "height");
                                first_media_image = Some((u.to_string(), w, h));
                            }
                        }
                    }
                }
            }
        }
    }

    // 封面判定：视频封面优先；正文已有同一个地址就不补；正文无图的图文帖用首张 media 图
    let cover: Option<ApiImage> = if let Some((cover_url, w, h)) = video_cover {
        if body_urls.iter().any(|u| u == &cover_url) {
            None
        } else {
            Some(ApiImage {
                url: cover_url,
                width: w,
                height: h,
                is_cover: true,
                note: Some(VIDEO_COVER_NOTE.to_string()),
            })
        }
    } else if body_images.is_empty() {
        first_media_image.map(|(url, w, h)| ApiImage {
            url,
            width: w,
            height: h,
            is_cover: true,
            note: None,
        })
    } else {
        None
    };

    let cover_index = cover.as_ref().map(|_| 0);
    let mut entries: Vec<ApiImage> = Vec::with_capacity(body_images.len() + if cover.is_some() { 1 } else { 0 });
    if let Some(c) = &cover {
        entries.push(c.clone());
    }

    for (n, b) in body_images.iter().enumerate() {
        let url = get_str(b, "url").unwrap_or_default();
        let original = get_str(b, "original");
        let w = get_i32(b, "width");
        let h = get_i32(b, "height");
        let req_url = image_url_of(url, original, original_images);
        let is_cover = cover.is_none() && n == 0;
        entries.push(ApiImage {
            url: req_url,
            width: w,
            height: h,
            is_cover,
            note: None,
        });
    }

    let detail_images = DetailImages {
        images: ContentImages { entries },
        cover_index,
    };

    // 2. 改写 JSON 结构
    // 顶层 images: count
    set_field(&mut root, "images", Value::Number(detail_images.images.count().to_string()));

    // 改写 item.media
    if let Some(pos) = root.iter().position(|(k, _)| k == "item") {
        if let Value::Object(ref mut item_fields) = root[pos].1 {
            if let Some(m_pos) = item_fields.iter().position(|(k, _)| k == "media") {
                if let Value::Array(ref mut media_arr) = item_fields[m_pos].1 {
                    for m in media_arr.iter_mut() {
                        if let Value::Object(ref mut m_fields) = m {
                            match get_str(m_fields, "type") {
                                Some("image") => remove_field(m_fields, "url"),
                                Some("video") => remove_field(m_fields, "cover"),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    // 改写 body.blocks
    if let Some(pos) = root.iter().position(|(k, _)| k == "body") {
        if let Value::Object(ref mut body_fields) = root[pos].1 {
            if let Some(b_pos) = body_fields.iter().position(|(k, _)| k == "blocks") {
                if let Value::Array(ref mut blocks_arr) = body_fields[b_pos].1 {
                    let mut out_blocks: Vec<Value> = Vec::with_capacity(blocks_arr.len() + 1);

                    // 视频封面补在最前面
                    if let Some(ref c) = cover {
                        out_blocks.push(Value::Object(image_block_fields(0, c, &detail_images, None)));
                    }

                    let mut img_seen = 0;
                    for el in blocks_arr.iter() {
                        if let Value::Object(b_fields) = el {
                            match get_str(b_fields, "type") {
                                Some("img") => {
                                    if let Some((idx, img)) = detail_images.image_for_img_block(img_seen) {
                                        img_seen += 1;
                                        out_blocks.push(Value::Object(image_block_fields(
                                            idx,
                                            img,
                                            &detail_images,
                                            Some(b_fields),
                                        )));
                                    } else {
                                        img_seen += 1;
                                        let mut safe = b_fields.clone();
                                        remove_field(&mut safe, "url");
                                        remove_field(&mut safe, "original");
                                        out_blocks.push(Value::Object(safe));
                                    }
                                }
                                Some("card") | Some("video") => {
                                    let mut safe = b_fields.clone();
                                    remove_field(&mut safe, "cover");
                                    out_blocks.push(Value::Object(safe));
                                }
                                _ => {
                                    out_blocks.push(el.clone());
                                }
                            }
                        } else {
                            out_blocks.push(el.clone());
                        }
                    }
                    body_fields[b_pos].1 = Value::Array(out_blocks);
                }
            }
        }
    }

    Ok((json::to_json_string(&Value::Object(root)), detail_images))
}

fn image_block_fields(
    idx: usize,
    image: &ApiImage,
    images: &DetailImages,
    original: Option<&Vec<(String, Value)>>,
) -> Vec<(String, Value)> {
    let mut fields = original.cloned().unwrap_or_default();
    set_field(&mut fields, "type", Value::String("img".to_string()));
    set_field(&mut fields, "idx", Value::Number(idx.to_string()));
    set_field(&mut fields, "isCover", Value::Bool(image.is_cover));
    remove_field(&mut fields, "url");
    remove_field(&mut fields, "original");

    if let Some(w) = image.width.filter(|w| *w > 0) {
        set_field(&mut fields, "width", Value::Number(w.to_string()));
    } else {
        remove_field(&mut fields, "width");
    }

    if let Some(h) = image.height.filter(|h| *h > 0) {
        set_field(&mut fields, "height", Value::Number(h.to_string()));
    } else {
        remove_field(&mut fields, "height");
    }

    if let Some(segs) = images.images.segments_at(idx).filter(|s| *s > 1) {
        set_field(&mut fields, "segments", Value::Number(segs.to_string()));
    } else {
        remove_field(&mut fields, "segments");
    }

    if let Some(ref note) = image.note {
        set_field(&mut fields, "note", Value::String(note.clone()));
    } else {
        remove_field(&mut fields, "note");
    }

    fields
}

/// 遍历评论树中的所有评论及其 `previewReplies`，提取图片清单。
pub fn extract_comments_images(comments_json: &str, original_images: bool) -> Vec<(String, ContentImages)> {
    let val = match json::parse(comments_json) {
        Ok(v) => v,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    match val {
        Value::Array(items) => {
            for item in &items {
                visit_comment(item, original_images, &mut out);
            }
        }
        Value::Object(_) => {
            visit_comment(&val, original_images, &mut out);
        }
        _ => {}
    }
    out
}

fn visit_comment(val: &Value, original_images: bool, out: &mut Vec<(String, ContentImages)>) {
    if let Value::Object(fields) = val {
        if let Some(id) = get_str(fields, "id") {
            let mut entries: Vec<ApiImage> = Vec::new();
            if let Some(content) = get_obj(fields, "content") {
                if let Some(blocks) = get_arr(content, "blocks") {
                    for b in blocks {
                        if let Value::Object(b_fields) = b {
                            if get_str(b_fields, "type") == Some("img") {
                                let url = get_str(b_fields, "url").unwrap_or_default();
                                let original = get_str(b_fields, "original");
                                let w = get_i32(b_fields, "width");
                                let h = get_i32(b_fields, "height");
                                entries.push(ApiImage {
                                    url: image_url_of(url, original, original_images),
                                    width: w,
                                    height: h,
                                    is_cover: false,
                                    note: None,
                                });
                            }
                        }
                    }
                }
            }
            out.push((id.to_string(), ContentImages { entries }));

            if let Some(replies) = get_arr(fields, "previewReplies") {
                for r in replies {
                    visit_comment(r, original_images, out);
                }
            }
        }
    }
}

// 辅助字段查找与修改工具
fn set_field(fields: &mut Vec<(String, Value)>, key: &str, val: Value) {
    if let Some(entry) = fields.iter_mut().find(|(k, _)| k == key) {
        entry.1 = val;
    } else {
        fields.push((key.to_string(), val));
    }
}

fn remove_field(fields: &mut Vec<(String, Value)>, key: &str) {
    fields.retain(|(k, _)| k != key);
}

fn get_obj<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a Vec<(String, Value)>> {
    fields.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        Value::Object(obj) => Some(obj),
        _ => None,
    })
}

fn get_arr<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a Vec<Value>> {
    fields.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        Value::Array(arr) => Some(arr),
        _ => None,
    })
}

fn get_str<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str())
}

fn get_i32(fields: &[(String, Value)], key: &str) -> Option<i32> {
    fields.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_i64()).map(|n| n as i32)
}


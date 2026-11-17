//! Feed and read through the shared 2.0 Rust operation options.

use super::*;
use crate::api::{Feed, Read, Since};
use crate::core::pool::Durability;

#[repr(C)]
pub struct plsm_read_options {
    struct_size: u32,
    start_kind: u32,
    start_value: u64,
    tags: *const batches::ViewStreamText,
    tags_len: usize,
}

#[repr(C)]
pub struct plsm_feed_options {
    pub(super) struct_size: u32,
    pub(super) durability: u32,
    pub(super) tags: *const batches::ViewStreamText,
    pub(super) tags_len: usize,
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_open(
    client: *mut plsm_client,
    pool_ref: *const c_char,
    create: u32,
    out: *mut *mut plsm_pool,
    error: *mut *mut plsm_error,
) -> i32 {
    if create > 1 {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("create must be zero or one"),
        );
    }
    open_pool_handle(client, pool_ref, create == 1, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_info(
    pool: *mut plsm_pool,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = pool
        .pool
        .info()
        .and_then(|info| write_wire(out, &pool_info_wire(&info)));
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_list(
    client: *mut plsm_client,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    let client = match borrow_client(client, error) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let result = client
        .client
        .list_pools()
        .and_then(|pools| write_wire(out, &pools.iter().map(pool_info_wire).collect::<Vec<_>>()));
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_delete(
    client: *mut plsm_client,
    pool_ref: *const c_char,
    error: *mut *mut plsm_error,
) -> i32 {
    let client = match borrow_client(client, error) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let pool_ref = match parse_pool_ref(pool_ref, error) {
        Ok(pool_ref) => pool_ref,
        Err(code) => return code,
    };
    client
        .client
        .delete_pool(&pool_ref)
        .map_or_else(|err| fail(error, err), |()| 0)
}

fn pool_info_wire(info: &crate::api::PoolInfo) -> crate::interface_wire::PoolInfoWire {
    crate::interface_wire::pool_info_wire!(info.path.display(), info)
}

fn write_wire(out: *mut plsm_buf, value: &impl serde::Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to serialize response")
            .with_source(err)
    })?;
    write_json_buf(out, bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_json(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_message is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let parsed = crate::core::lite3::ParsedJson::parse(unsafe {
            feed_input_bytes(bytes, len, "json_bytes")
        }?)?;
        let (durability, tags) = feed_arguments(options)?;
        let tags = tags.unwrap_or_default();
        let (seq, time) = append_parsed(&mut pool.pool, &parsed, &tags, durability)?;
        let wire = crate::interface_wire::MessageWire::new(
            seq,
            crate::api::format_ts(time)?,
            &tags,
            parsed.value(),
        );
        write_wire(out, &wire)
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_lite3(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_message is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let message = pool.pool.feed_lite3(
            unsafe { feed_input_bytes(bytes, len, "lite3_bytes") }?,
            parse_feed(options)?,
        )?;
        write_message_buf(out, message)
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_json_receipt(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_feed_receipt,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_receipt is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let parsed = crate::core::lite3::ParsedJson::parse(unsafe {
            feed_input_bytes(bytes, len, "json_bytes")
        }?)?;
        let (durability, tags) = feed_arguments(options)?;
        let (seq, time) = append_parsed(
            &mut pool.pool,
            &parsed,
            &tags.unwrap_or_default(),
            durability,
        )?;
        unsafe { out.write(plsm_feed_receipt { seq, time }) };
        Ok(())
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_lite3_receipt(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_feed_receipt,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_receipt is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let receipt = pool.pool.feed_lite3_receipt(
            unsafe { feed_input_bytes(bytes, len, "lite3_bytes") }?,
            parse_feed(options)?,
        )?;
        unsafe {
            out.write(plsm_feed_receipt {
                seq: receipt.seq,
                time: receipt.time,
            })
        };
        Ok(())
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

fn parse_feed(options: *const plsm_feed_options) -> Result<Feed, Error> {
    let (durability, tags) = feed_arguments(options)?;
    let feed = Feed::new().durability(if durability == 0 {
        Durability::Fast
    } else {
        Durability::Flush
    });
    Ok(match tags {
        Some(tags) => feed.tags(tags),
        None => feed,
    })
}

pub(crate) fn feed_arguments<'a>(
    options: *const plsm_feed_options,
) -> Result<(u32, Option<Vec<&'a str>>), Error> {
    if options.is_null() {
        return Ok((0, None));
    }
    let size = unsafe { ptr::addr_of!((*options).struct_size).read() } as usize;
    if size < std::mem::size_of::<plsm_feed_options>() {
        return Err(Error::new(ErrorKind::Usage).with_message("feed options are too small"));
    }
    let options = unsafe { &*options };
    if options.durability > 1 {
        return Err(Error::new(ErrorKind::Usage).with_message("durability must be fast or flush"));
    }
    let tags = if options.tags.is_null() && options.tags_len == 0 {
        None
    } else {
        Some(unsafe { batches::tag_spans(options.tags, options.tags_len) }?)
    };
    Ok((options.durability, tags))
}

fn parse_read(options: *const plsm_read_options) -> Result<Read, Error> {
    if options.is_null() {
        return Ok(Read::new());
    }
    let size = unsafe { ptr::addr_of!((*options).struct_size).read() } as usize;
    if size < std::mem::size_of::<plsm_read_options>() {
        return Err(Error::new(ErrorKind::Usage).with_message("read options are too small"));
    }
    let options = unsafe { &*options };
    let read = match options.start_kind {
        0 => Read::new(),
        1 => Read::since(Since::Oldest),
        2 => Read::since(Since::Seq(options.start_value)),
        3 => Read::since(Since::At(options.start_value)),
        4 => Read::since(Since::Ago(Duration::from_nanos(options.start_value))),
        5 => Read::last(options.start_value),
        _ => return Err(Error::new(ErrorKind::Usage).with_message("invalid read start kind")),
    };
    Ok(read.tags(unsafe { batches::tag_spans(options.tags, options.tags_len) }?))
}

fn read_state(
    handle: *mut plsm_pool,
    options: *const plsm_read_options,
    live: bool,
    error: *mut *mut plsm_error,
) -> Result<(Pool, TailState, Arc<AtomicBool>), i32> {
    let handle = borrow_pool(handle, error)?;
    let options = parse_read(options).map_err(|err| fail(error, err))?;
    // Streams own their mapping. Choose the start on that mapping before returning.
    let pool = Pool::open(handle.pool.path()).map_err(|err| fail(error, err))?;
    let cancel = Arc::new(AtomicBool::new(false));
    let state = pool
        .read_state(options.cancel(cancel.clone()), live)
        .map_err(|err| fail(error, err))?;
    Ok((pool, state, cancel))
}

fn open_text(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    live: bool,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_stream is null"),
        );
    }
    let (pool, state, cancel) = match read_state(pool, options, live, error) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let stream = Box::new(plsm_stream {
        pool,
        state: Mutex::new(state),
        cancel,
    });
    unsafe {
        *out = Box::into_raw(stream);
    }
    0
}

fn open_binary(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    live: bool,
    out: *mut *mut plsm_lite3_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_stream is null"),
        );
    }
    let (pool, state, cancel) = match read_state(pool, options, live, error) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let stream = Box::new(plsm_lite3_stream {
        pool,
        state: Mutex::new(state),
        cancel,
    });
    unsafe {
        *out = Box::into_raw(stream);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_follow(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_text(pool, options, true, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_follow_view(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_text(pool, options, true, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_fetch_view(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_text(pool, options, false, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_fetch(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_text(pool, options, false, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_follow_lite3(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_lite3_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_binary(pool, options, true, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_fetch_lite3(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_lite3_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_binary(pool, options, false, out, error)
}

// A successful UDP feed only confirms local socket admission.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_feed_json(
    pool: *mut plsm_udp_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    error: *mut *mut plsm_error,
) -> i32 {
    let result = (|| {
        let pool = unsafe { pool.as_ref() }
            .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP pool is null"))?;
        let data: serde_json::Value =
            serde_json::from_slice(unsafe { feed_input_bytes(bytes, len, "json_bytes") }?)
                .map_err(|err| {
                    Error::new(ErrorKind::Usage)
                        .with_message("invalid JSON object")
                        .with_source(err)
                })?;
        pool.pool.feed(&data, parse_feed(options)?)
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_read(
    pool: *mut plsm_udp_pool,
    options: *const plsm_read_options,
    live: u32,
    views: u32,
    out: *mut *mut plsm_udp_follow,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() || live > 1 || views > 1 {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("invalid UDP reader arguments"),
        );
    }
    let result = (|| {
        let handle = unsafe { pool.as_ref() }
            .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP pool is null"))?;
        let read = parse_read(options)?;
        let follow = match (live == 1, views == 1) {
            (true, false) => UdpFollower::Raw(handle.pool.follow(read)?),
            (false, false) => UdpFollower::Raw(handle.pool.fetch(read)?),
            (true, true) => UdpFollower::Views(handle.pool.follow_view(read)?),
            (false, true) => UdpFollower::Views(handle.pool.fetch_view(read)?),
        };
        unsafe {
            *out = Box::into_raw(Box::new(plsm_udp_follow {
                follow: Mutex::new(follow),
                cancelled: AtomicBool::new(false),
            }));
        }
        Ok(())
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

//! Plain local time, provided by the host through the `Clock` seam; the
//! timezone conversion is host business (bionic's `localtime_r` applies
//! `persist.sys.timezone` itself on Android), never the core's.

pub struct LocalTime {
    pub hour: i32,
    pub min: i32,
    pub mday: i32,
    /// 0-based, as `tm_mon` is.
    pub mon: i32,
    /// 0 = Sunday, as `tm_wday` is.
    pub wday: i32,
}

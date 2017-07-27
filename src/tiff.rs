//
// Copyright (c) 2016 KAMADA Ken'ichi.
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
// 1. Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in the
//    documentation and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY THE AUTHOR AND CONTRIBUTORS ``AS IS'' AND
// ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED.  IN NO EVENT SHALL THE AUTHOR OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS
// OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
// HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT
// LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY
// OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF
// SUCH DAMAGE.
//

use std::fmt;

use endian::{Endian, BigEndian, LittleEndian};
use error::Error;
use tag;
use tag_priv::{Context, Tag};
use value::Value;
use value::get_type_info;
use util::atou16;

// TIFF header magic numbers [EXIF23 4.5.2].
const TIFF_BE: u16 = 0x4d4d;
const TIFF_LE: u16 = 0x4949;
const TIFF_FORTY_TWO: u16 = 0x002a;
pub const TIFF_BE_SIG: [u8; 4] = [0x4d, 0x4d, 0x00, 0x2a];
pub const TIFF_LE_SIG: [u8; 4] = [0x49, 0x49, 0x2a, 0x00];

/// A TIFF field.
#[derive(Debug)]
pub struct Field<'a> {
    /// The tag of this field.
    pub tag: Tag,
    /// False for the primary image and true for the thumbnail.
    pub thumbnail: bool,
    /// The value of this field.
    pub value: Value<'a>,
}

/// Parse the Exif attributes in the TIFF format.
///
/// Returns a Vec of Exif fields and a bool.
/// The boolean value is true if the data is little endian.
/// If an error occurred, `exif::Error` is returned.
pub fn parse_exif(data: &[u8]) -> Result<(Vec<Field>, bool), Error> {
    // Check the byte order and call the real parser.
    if data.len() < 8 {
        return Err(Error::InvalidFormat("Truncated TIFF header"));
    }
    match BigEndian::loadu16(data, 0) {
        TIFF_BE => parse_exif_sub::<BigEndian>(data).map(|v| (v, false)),
        TIFF_LE => parse_exif_sub::<LittleEndian>(data).map(|v| (v, true)),
        _ => Err(Error::InvalidFormat("Invalid TIFF byte order")),
    }
}

fn parse_exif_sub<E>(data: &[u8])
                     -> Result<Vec<Field>, Error> where E: Endian {
    // Parse the rest of the header (42 and the IFD offset).
    if E::loadu16(data, 2) != TIFF_FORTY_TWO {
        return Err(Error::InvalidFormat("Invalid forty two"));
    }
    let ifd_offset = E::loadu32(data, 4) as usize;
    let mut fields = Vec::new();
    try!(parse_ifd::<E>(&mut fields, data, ifd_offset, Context::Tiff, false));
    Ok(fields)
}

// Parse IFD [EXIF23 4.6.2].
fn parse_ifd<'a, E>(fields: &mut Vec<Field<'a>>, data: &'a [u8],
                    offset: usize, ctx: Context, thumbnail: bool)
                    -> Result<(), Error> where E: Endian {
    // Count (the number of the entries).
    if data.len() < offset || data.len() - offset < 2 {
        return Err(Error::InvalidFormat("Truncated IFD count"));
    }
    let count = E::loadu16(data, offset) as usize;

    // Array of entries.  (count * 12) never overflow.
    if data.len() - offset - 2 < count * 12 {
        return Err(Error::InvalidFormat("Truncated IFD"));
    }
    for i in 0..count as usize {
        let tag = E::loadu16(data, offset + 2 + i * 12);
        let typ = E::loadu16(data, offset + 2 + i * 12 + 2);
        let cnt = E::loadu32(data, offset + 2 + i * 12 + 4) as usize;
        let valofs_at = offset + 2 + i * 12 + 8;
        let (unitlen, parser) = get_type_info::<E>(typ);
        let vallen = try!(unitlen.checked_mul(cnt).ok_or(
            Error::InvalidFormat("Invalid entry count")));
        let val;
        if unitlen == 0 {
            val = Value::Unknown(typ, cnt as u32, valofs_at as u32);
        } else if vallen <= 4 {
            val = parser(data, valofs_at, cnt);
        } else {
            let ofs = E::loadu32(data, valofs_at) as usize;
            if data.len() < ofs || data.len() - ofs < vallen {
                return Err(Error::InvalidFormat("Truncated field value"));
            }
            val = parser(data, ofs, cnt);
        }

        // No infinite recursion will occur because the context is not
        // recursively defined.
        let tag = Tag(ctx, tag);
        match tag {
            tag::ExifIFDPointer => try!(parse_child_ifd::<E>(
                fields, data, &val, Context::Exif, thumbnail)),
            tag::GPSInfoIFDPointer => try!(parse_child_ifd::<E>(
                fields, data, &val, Context::Gps, thumbnail)),
            tag::InteropIFDPointer => try!(parse_child_ifd::<E>(
                fields, data, &val, Context::Interop, thumbnail)),
            _ => fields.push(Field { tag: tag, thumbnail: thumbnail,
                                     value: val }),
        }
    }

    // Offset to the next IFD.
    if data.len() - offset - 2 - count * 12 < 4 {
        return Err(Error::InvalidFormat("Truncated next IFD offset"));
    }
    let next_ifd_offset = E::loadu32(data, offset + 2 + count * 12) as usize;
    if next_ifd_offset == 0 {
        return Ok(());
    }
    if ctx != Context::Tiff || thumbnail {
        return Err(Error::InvalidFormat("Unexpected next IFD"));
    }
    parse_ifd::<E>(fields, data, next_ifd_offset, Context::Tiff, true)
}

fn parse_child_ifd<'a, E>(fields: &mut Vec<Field<'a>>, data: &'a [u8],
                          pointer: &Value, ctx: Context, thumbnail: bool)
                          -> Result<(), Error> where E: Endian {
    // A pointer field has type == LONG and count == 1, so the
    // value (IFD offset) must be embedded in the "value offset"
    // element of the field.
    let ofs = try!(pointer.get_uint(0).ok_or(
        Error::InvalidFormat("Invalid pointer"))) as usize;
    parse_ifd::<E>(fields, data, ofs, ctx, thumbnail)
}

pub fn is_tiff(buf: &[u8]) -> bool {
    buf.starts_with(&TIFF_BE_SIG) || buf.starts_with(&TIFF_LE_SIG)
}

/// A struct used to parse a DateTime field.
///
/// # Examples
/// ```
/// use exif::DateTime;
/// let dt = DateTime::from_ascii(b"2016:05:04 03:02:01").unwrap();
/// assert_eq!(dt.year, 2016);
/// assert_eq!(format!("{}", dt), "2016-05-04 03:02:01");
/// ```
#[derive(Debug)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// Parse an ASCII data of a DateTime field.  The range of a number
    /// is not validated, so, for example, 13 may be returned as the month.
    pub fn from_ascii(data: &[u8]) -> Result<DateTime, Error> {
        if data == b"    :  :     :  :  " || data == b"                   " {
            return Err(Error::BlankValue("DateTime is blank"));
        } else if data.len() < 19 {
            return Err(Error::InvalidFormat("DateTime too short"));
        } else if !(data[4] == b':' && data[7] == b':' && data[10] == b' ' &&
                    data[13] == b':' && data[16] == b':') {
            return Err(Error::InvalidFormat("Invalid DateTime delimiter"));
        }
        Ok(DateTime {
            year: try!(atou16(&data[0..4])),
            month: try!(atou16(&data[5..7])) as u8,
            day: try!(atou16(&data[8..10])) as u8,
            hour: try!(atou16(&data[11..13])) as u8,
            minute: try!(atou16(&data[14..16])) as u8,
            second: try!(atou16(&data[17..19])) as u8,
        })
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
               self.year, self.month, self.day,
               self.hour, self.minute, self.second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Before the error is returned, the IFD is parsed twice as the
    // 0th and 1st IFDs.
    #[test]
    fn inf_loop_by_next() {
        let data = b"MM\0\x2a\0\0\0\x08\
                     \0\x01\x01\0\0\x03\0\0\0\x01\0\x14\0\0\0\0\0\x08";
        assert_err_pat!(parse_exif(data),
                        Error::InvalidFormat("Unexpected next IFD"));
    }

    #[test]
    fn unknown_field() {
        let data = b"MM\0\x2a\0\0\0\x08\
                     \0\x01\x01\0\xff\xff\0\0\0\x01\0\x14\0\0\0\0\0\0";
        let (v, _) = parse_exif(data).unwrap();
        assert_eq!(v.len(), 1);
        assert_pat!(v[0].value, Value::Unknown(0xffff, 1, 0x12));
    }
}

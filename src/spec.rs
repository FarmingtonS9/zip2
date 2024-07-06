#![macro_use]

use crate::result::{ZipError, ZipResult};
use core::mem;
use core::mem::align_of;
use memchr::memmem::FinderRev;
use std::io;
use std::io::prelude::*;
use std::mem::size_of_val;
use std::path::{Component, Path, MAIN_SEPARATOR};
use std::rc::Rc;

/// "Magic" header values used in the zip spec to locate metadata records.
///
/// These values currently always take up a fixed four bytes, so we can parse and wrap them in this
/// struct to enforce some small amount of type safety.
#[derive(Copy, Clone, Debug, PartialOrd, Ord, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct Magic(u32);

impl Magic {
    pub const fn literal(x: u32) -> Self {
        Self(x)
    }

    #[inline(always)]
    pub const fn from_le_bytes(bytes: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(bytes))
    }

    #[inline(always)]
    pub fn from_first_le_bytes(data: &[u8]) -> Self {
        let first_bytes: [u8; 4] = data[..mem::size_of::<Self>()].try_into().unwrap();
        Self::from_le_bytes(first_bytes)
    }

    #[inline(always)]
    pub const fn to_le_bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    #[allow(clippy::wrong_self_convention)]
    #[inline(always)]
    pub fn from_le(self) -> Self {
        Self(u32::from_le(self.0))
    }

    #[allow(clippy::wrong_self_convention)]
    #[inline(always)]
    pub fn to_le(self) -> Self {
        Self(u32::to_le(self.0))
    }

    pub const LOCAL_FILE_HEADER_SIGNATURE: Self = Self::literal(0x04034b50);
    pub const CENTRAL_DIRECTORY_HEADER_SIGNATURE: Self = Self::literal(0x02014b50);
    pub const CENTRAL_DIRECTORY_END_SIGNATURE: Self = Self::literal(0x06054b50);
    pub const ZIP64_CENTRAL_DIRECTORY_END_SIGNATURE: Self = Self::literal(0x06064b50);
    pub const ZIP64_CENTRAL_DIRECTORY_END_LOCATOR_SIGNATURE: Self = Self::literal(0x07064b50);
}

/// Similar to [`Magic`], but used for extra field tags as per section 4.5.3 of APPNOTE.TXT.
#[derive(Copy, Clone, Debug, PartialOrd, Ord, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct ExtraFieldMagic(u16);

/* TODO: maybe try to use this for parsing extra fields as well as writing them? */
#[allow(dead_code)]
impl ExtraFieldMagic {
    pub const fn literal(x: u16) -> Self {
        Self(x)
    }

    #[inline(always)]
    pub const fn from_le_bytes(bytes: [u8; 2]) -> Self {
        Self(u16::from_le_bytes(bytes))
    }

    #[inline(always)]
    pub const fn to_le_bytes(self) -> [u8; 2] {
        self.0.to_le_bytes()
    }

    #[allow(clippy::wrong_self_convention)]
    #[inline(always)]
    pub fn from_le(self) -> Self {
        Self(u16::from_le(self.0))
    }

    #[allow(clippy::wrong_self_convention)]
    #[inline(always)]
    pub fn to_le(self) -> Self {
        Self(u16::to_le(self.0))
    }

    pub const ZIP64_EXTRA_FIELD_TAG: Self = Self::literal(0x0001);
}

/// This should be equal to `0xFFFFFFFF`.
pub const ZIP64_BYTES_THR: u64 = u32::MAX as u64;
pub const ZIP64_ENTRY_THR: usize = u16::MAX as usize;

pub(crate) trait FixedSizeBlock: Sized + Copy {
    const MAGIC: Magic;

    fn magic(self) -> Magic;

    const WRONG_MAGIC_ERROR: ZipError;

    /* TODO: use smallvec? */
    fn interpret(bytes: &[u8]) -> ZipResult<Self> {
        if bytes.len() != mem::size_of::<Self>() {
            return Err(ZipError::InvalidArchive("Block is wrong size"));
        }
        let block_ptr: *const Self = bytes.as_ptr().cast();

        // If alignment could be more than 1, we'd have to use read_unaligned() below
        debug_assert_eq!(align_of::<Self>(), 1);

        let block = unsafe { block_ptr.read() }.from_le();
        if block.magic() != Self::MAGIC {
            return Err(Self::WRONG_MAGIC_ERROR);
        }
        Ok(block)
    }

    #[allow(clippy::wrong_self_convention)]
    fn from_le(self) -> Self;

    fn parse<T: Read>(reader: &mut T) -> ZipResult<Self> {
        let mut block = vec![0u8; mem::size_of::<Self>()].into_boxed_slice();
        reader.read_exact(&mut block)?;
        Self::interpret(&block)
    }

    fn encode(self) -> Box<[u8]> {
        self.to_le().serialize()
    }

    fn to_le(self) -> Self;

    /* TODO: use Box<[u8; mem::size_of::<Self>()]> when generic_const_exprs are stabilized! */
    fn serialize(self) -> Box<[u8]> {
        /* TODO: use Box::new_zeroed() when stabilized! */
        /* TODO: also consider using smallvec! */

        // If alignment could be more than 1, we'd have to use write_unaligned() below
        debug_assert_eq!(align_of::<Self>(), 1);

        let mut out_block = vec![0u8; mem::size_of::<Self>()].into_boxed_slice();
        let out_ptr: *mut Self = out_block.as_mut_ptr().cast();
        unsafe {
            out_ptr.write(self);
        }
        out_block
    }

    fn write<T: Write>(self, writer: &mut T) -> ZipResult<()> {
        let block = self.encode();
        writer.write_all(&block)?;
        Ok(())
    }
}

/// Convert all the fields of a struct *from* little-endian representations.
macro_rules! from_le {
    ($obj:ident, $field:ident, $type:ty) => {
        $obj.$field = <$type>::from_le($obj.$field);
    };
    ($obj:ident, [($field:ident, $type:ty) $(,)?]) => {
        from_le![$obj, $field, $type];
    };
    ($obj:ident, [($field:ident, $type:ty), $($rest:tt),+ $(,)?]) => {
        from_le![$obj, $field, $type];
        from_le!($obj, [$($rest),+]);
    };
}

/// Convert all the fields of a struct *into* little-endian representations.
macro_rules! to_le {
    ($obj:ident, $field:ident, $type:ty) => {
        $obj.$field = <$type>::to_le($obj.$field);
    };
    ($obj:ident, [($field:ident, $type:ty) $(,)?]) => {
        to_le![$obj, $field, $type];
    };
    ($obj:ident, [($field:ident, $type:ty), $($rest:tt),+ $(,)?]) => {
        to_le![$obj, $field, $type];
        to_le!($obj, [$($rest),+]);
    };
}

/* TODO: derive macro to generate these fields? */
/// Implement `from_le()` and `to_le()`, providing the field specification to both macros
/// and methods.
macro_rules! to_and_from_le {
    ($($args:tt),+ $(,)?) => {
        #[inline(always)]
        fn from_le(mut self) -> Self {
            from_le![self, [$($args),+]];
            self
        }
        #[inline(always)]
        fn to_le(mut self) -> Self {
            to_le![self, [$($args),+]];
            self
        }
    };
}

#[derive(Copy, Clone, Debug)]
#[repr(packed)]
pub(crate) struct Zip32CDEBlock {
    magic: Magic,
    pub disk_number: u16,
    pub disk_with_central_directory: u16,
    pub number_of_files_on_this_disk: u16,
    pub number_of_files: u16,
    pub central_directory_size: u32,
    pub central_directory_offset: u32,
    pub zip_file_comment_length: u16,
}

impl FixedSizeBlock for Zip32CDEBlock {
    const MAGIC: Magic = Magic::CENTRAL_DIRECTORY_END_SIGNATURE;

    #[inline(always)]
    fn magic(self) -> Magic {
        self.magic
    }

    const WRONG_MAGIC_ERROR: ZipError =
        ZipError::InvalidArchive("Invalid digital signature header");

    to_and_from_le![
        (magic, Magic),
        (disk_number, u16),
        (disk_with_central_directory, u16),
        (number_of_files_on_this_disk, u16),
        (number_of_files, u16),
        (central_directory_size, u32),
        (central_directory_offset, u32),
        (zip_file_comment_length, u16)
    ];
}

#[derive(Debug)]
pub(crate) struct Zip32CentralDirectoryEnd {
    pub disk_number: u16,
    pub disk_with_central_directory: u16,
    pub number_of_files_on_this_disk: u16,
    pub number_of_files: u16,
    pub central_directory_size: u32,
    pub central_directory_offset: u32,
    pub zip_file_comment: Box<[u8]>,
}

impl Zip32CentralDirectoryEnd {
    fn block_and_comment(self) -> ZipResult<(Zip32CDEBlock, Box<[u8]>)> {
        let Self {
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
            zip_file_comment,
        } = self;
        let block = Zip32CDEBlock {
            magic: Zip32CDEBlock::MAGIC,
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
            zip_file_comment_length: zip_file_comment
                .len()
                .try_into()
                .map_err(|_| ZipError::InvalidArchive("File comment must be less than 64 KiB"))?,
        };
        Ok((block, zip_file_comment))
    }

    pub fn parse<T: Read>(reader: &mut T) -> ZipResult<Zip32CentralDirectoryEnd> {
        let Zip32CDEBlock {
            // magic,
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
            zip_file_comment_length,
            ..
        } = Zip32CDEBlock::parse(reader)?;

        let mut zip_file_comment = vec![0u8; zip_file_comment_length as usize].into_boxed_slice();
        reader.read_exact(&mut zip_file_comment)?;

        Ok(Zip32CentralDirectoryEnd {
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
            zip_file_comment,
        })
    }

    #[allow(clippy::type_complexity)]
    pub fn find_and_parse<T: Read + Seek>(
        reader: &mut T,
    ) -> ZipResult<Box<[(Rc<Zip32CentralDirectoryEnd>, u64)]>> {
        let mut results = vec![];
        let file_length = reader.seek(io::SeekFrom::End(0))?;
        if file_length < mem::size_of::<Zip32CDEBlock>() as u64 {
            return Err(ZipError::InvalidArchive("Invalid zip header"));
        }
        let last_chunk_start = reader.seek(io::SeekFrom::End(
            -(std::cmp::min(file_length as usize, HEADER_SIZE + u16::MAX as usize) as i64),
        ))?;
        let mut last_chunk = Vec::with_capacity(HEADER_SIZE + u16::MAX as usize);
        reader.read_to_end(&mut last_chunk)?;

        if last_chunk.len() < HEADER_SIZE {
            return Err(ZipError::InvalidArchive("Invalid zip header"));
        }

        let mut pos = last_chunk.len() - HEADER_SIZE;
        loop {
            if (&last_chunk[pos..]).read_u32_le()? == CENTRAL_DIRECTORY_END_SIGNATURE {
                let cde_start_pos = last_chunk_start + pos as u64;
                if let Ok(end_header) = CentralDirectoryEnd::parse(&mut &last_chunk[pos..]) {
                    return Ok((end_header, cde_start_pos));

        let search_lower_bound = 0;

        const END_WINDOW_SIZE: usize = 512;
        /* TODO: use static_assertions!() */
        debug_assert!(END_WINDOW_SIZE > mem::size_of::<Magic>());

        const SIG_BYTES: [u8; mem::size_of::<Magic>()] =
            Magic::CENTRAL_DIRECTORY_END_SIGNATURE.to_le_bytes();
        let finder = FinderRev::new(&SIG_BYTES);

        let mut window_start: u64 = file_length.saturating_sub(END_WINDOW_SIZE as u64);
        let mut window = [0u8; END_WINDOW_SIZE];
        while window_start >= search_lower_bound {
            /* Go to the start of the window in the file. */
            reader.seek(io::SeekFrom::Start(window_start))?;

            /* Identify how many bytes to read (this may be less than the window size for files
             * smaller than END_WINDOW_SIZE). */
            let end = (window_start + END_WINDOW_SIZE as u64).min(file_length);
            let cur_len = (end - window_start) as usize;
            debug_assert!(cur_len > 0);
            debug_assert!(cur_len <= END_WINDOW_SIZE);
            let cur_window: &mut [u8] = &mut window[..cur_len];
            /* Read the window into the bytes! */
            reader.read_exact(cur_window)?;

            /* Find instances of the magic signature. */
            for offset in finder.rfind_iter(cur_window) {
                let cde_start_pos = window_start + offset as u64;
                reader.seek(io::SeekFrom::Start(cde_start_pos))?;
                /* Drop any headers that don't parse. */
                if let Ok(cde) = Self::parse(reader) {
                    results.push((Rc::new(cde), cde_start_pos));
                }
            }

            /* We always want to make sure we go allllll the way back to the start of the file if
             * we can't find it elsewhere. However, our `while` condition doesn't check that. So we
             * avoid infinite looping by checking at the end of the loop. */
            if window_start == search_lower_bound {
                break;
            }

            window_start = window_start
                /* This should never happen, but make sure we don't go past the end of the file. */
                .min(file_length);
            window_start = window_start
                .saturating_sub(
                    /* Shift the window by END_WINDOW_SIZE bytes, but make sure to cover matches that
                     * overlap our nice neat window boundaries!*/
                    (END_WINDOW_SIZE - mem::size_of::<Magic>()) as u64,
                )
                /* This will never go below the value of `search_lower_bound`, so we have a special
                 * `if window_start == search_lower_bound` check above. */
                .max(search_lower_bound);
        }
        if results.is_empty() {
            Err(ZipError::InvalidArchive(
                "Could not find central directory end",
            ))
        } else {
            Ok(results.into_boxed_slice())
        }
    }

    pub fn write<T: Write>(self, writer: &mut T) -> ZipResult<()> {
        let (block, comment) = self.block_and_comment()?;
        block.write(writer)?;
        writer.write_all(&comment)?;
        Ok(())
    }
}

#[derive(Copy, Clone)]
#[repr(packed)]
pub(crate) struct Zip64CDELocatorBlock {
    magic: Magic,
    pub disk_with_central_directory: u32,
    pub end_of_central_directory_offset: u64,
    pub number_of_disks: u32,
}

impl FixedSizeBlock for Zip64CDELocatorBlock {
    const MAGIC: Magic = Magic::ZIP64_CENTRAL_DIRECTORY_END_LOCATOR_SIGNATURE;

    #[inline(always)]
    fn magic(self) -> Magic {
        self.magic
    }

    const WRONG_MAGIC_ERROR: ZipError =
        ZipError::InvalidArchive("Invalid zip64 locator digital signature header");

    to_and_from_le![
        (magic, Magic),
        (disk_with_central_directory, u32),
        (end_of_central_directory_offset, u64),
        (number_of_disks, u32),
    ];
}

pub(crate) struct Zip64CentralDirectoryEndLocator {
    pub disk_with_central_directory: u32,
    pub end_of_central_directory_offset: u64,
    pub number_of_disks: u32,
}

impl Zip64CentralDirectoryEndLocator {
    pub fn parse<T: Read>(reader: &mut T) -> ZipResult<Zip64CentralDirectoryEndLocator> {
        let Zip64CDELocatorBlock {
            // magic,
            disk_with_central_directory,
            end_of_central_directory_offset,
            number_of_disks,
            ..
        } = Zip64CDELocatorBlock::parse(reader)?;

        Ok(Zip64CentralDirectoryEndLocator {
            disk_with_central_directory,
            end_of_central_directory_offset,
            number_of_disks,
        })
    }

    pub fn block(self) -> Zip64CDELocatorBlock {
        let Self {
            disk_with_central_directory,
            end_of_central_directory_offset,
            number_of_disks,
        } = self;
        Zip64CDELocatorBlock {
            magic: Zip64CDELocatorBlock::MAGIC,
            disk_with_central_directory,
            end_of_central_directory_offset,
            number_of_disks,
        }
    }

    pub fn write<T: Write>(self, writer: &mut T) -> ZipResult<()> {
        self.block().write(writer)
    }
}

#[derive(Copy, Clone)]
#[repr(packed)]
pub(crate) struct Zip64CDEBlock {
    magic: Magic,
    pub record_size: u64,
    pub version_made_by: u16,
    pub version_needed_to_extract: u16,
    pub disk_number: u32,
    pub disk_with_central_directory: u32,
    pub number_of_files_on_this_disk: u64,
    pub number_of_files: u64,
    pub central_directory_size: u64,
    pub central_directory_offset: u64,
}

impl FixedSizeBlock for Zip64CDEBlock {
    const MAGIC: Magic = Magic::ZIP64_CENTRAL_DIRECTORY_END_SIGNATURE;

    fn magic(self) -> Magic {
        self.magic
    }

    const WRONG_MAGIC_ERROR: ZipError =
        ZipError::InvalidArchive("Invalid digital signature header");

    to_and_from_le![
        (magic, Magic),
        (record_size, u64),
        (version_made_by, u16),
        (version_needed_to_extract, u16),
        (disk_number, u32),
        (disk_with_central_directory, u32),
        (number_of_files_on_this_disk, u64),
        (number_of_files, u64),
        (central_directory_size, u64),
        (central_directory_offset, u64),
    ];
}

pub(crate) struct Zip64CentralDirectoryEnd {
    pub version_made_by: u16,
    pub version_needed_to_extract: u16,
    pub disk_number: u32,
    pub disk_with_central_directory: u32,
    pub number_of_files_on_this_disk: u64,
    pub number_of_files: u64,
    pub central_directory_size: u64,
    pub central_directory_offset: u64,
    //pub extensible_data_sector: Vec<u8>, <-- We don't do anything with this at the moment.
}

impl Zip64CentralDirectoryEnd {
    pub fn parse<T: Read>(reader: &mut T) -> ZipResult<Zip64CentralDirectoryEnd> {
        let Zip64CDEBlock {
            // record_size,
            version_made_by,
            version_needed_to_extract,
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
            ..
        } = Zip64CDEBlock::parse(reader)?;
        Ok(Self {
            version_made_by,
            version_needed_to_extract,
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
        })
    }

    pub fn find_and_parse<T: Read + Seek>(
        reader: &mut T,
        search_lower_bound: u64,
        search_upper_bound: u64,
    ) -> ZipResult<Vec<(Zip64CentralDirectoryEnd, u64)>> {
        const END_WINDOW_SIZE: usize = 2048;
        /* TODO: use static_assertions!() */
        debug_assert!(END_WINDOW_SIZE > mem::size_of::<Magic>());
        let mut results = Vec::new();
        let mut pos = search_upper_bound;
      

        const HEADER_SIZE: usize = 56; /* does not include comment */

        let mut buffer = Vec::new();
        while pos >= nominal_offset {
            reader.seek(io::SeekFrom::Start(pos))?;

            buffer.resize(
                std::cmp::min(4096, HEADER_SIZE + (search_upper_bound - pos) as usize),
                0u8,
            );
            let mut window_start: u64 = search_upper_bound
                .saturating_sub(END_WINDOW_SIZE as u64)
                .max(search_lower_bound);
            let mut window = [0u8; END_WINDOW_SIZE];
            while window_start >= search_lower_bound {
                reader.seek(io::SeekFrom::Start(window_start))?;

                /* Identify how many bytes to read (this may be less than the window size for files
                 * smaller than END_WINDOW_SIZE). */
                let end = (window_start + END_WINDOW_SIZE as u64).min(search_upper_bound);

                debug_assert!(end >= window_start);
                let cur_len = (end - window_start) as usize;
                if cur_len == 0 {
                    break;
                }
                debug_assert!(cur_len <= END_WINDOW_SIZE);
                let cur_window: &mut [u8] = &mut window[..cur_len];
                /* Read the window into the bytes! */
                reader.read_exact(cur_window)?;
                reader.read_exact(&mut buffer)?;
                let mut has_end_sig = false;
                const SIG_BYTES: [u8; mem::size_of::<Magic>()] =
                    Magic::ZIP64_CENTRAL_DIRECTORY_END_SIGNATURE.to_le_bytes();
                let finder = FinderRev::new(&SIG_BYTES);
                /* Find instances of the magic signature. */
                for offset in finder.rfind_iter(cur_window) {
                    has_end_sig = true;
                    let cde_start_pos = window_start + offset as u64;
                    reader.seek(io::SeekFrom::Start(cde_start_pos))?;

                    debug_assert!(cde_start_pos >= search_lower_bound);
                    let archive_offset = cde_start_pos - search_lower_bound;
                    let cde = Self::parse(reader)?;

                    results.push((cde, archive_offset));
                }

                /* We always want to make sure we go allllll the way back to the start of the file if
                 * we can't find it elsewhere. However, our `while` condition doesn't check that. So we
                 * avoid infinite looping by checking at the end of the loop. */
                if window_start == search_lower_bound {
                    break;
                }
                /* Shift the window by END_WINDOW_SIZE bytes, but make sure to cover matches that
                 * overlap our nice neat window boundaries! */
                window_start = (window_start
                    /* NB: To catch matches across window boundaries, we need to make our blocks overlap
                     * by the width of the pattern to match. */
                    + mem::size_of::<Magic>() as u64)
                    /* This may never happen, but make sure we don't go past the end of the specified
                     * range. */
                    .min(search_upper_bound);
                window_start = window_start
                    .saturating_sub(
                        /* Shift the window upon each iteration so we search END_WINDOW_SIZE bytes at
                         * once (unless limited by search_upper_bound). */
                        END_WINDOW_SIZE as u64,
                    )
                    /* This will never go below the value of `search_lower_bound`, so we have a special
                     * `if window_start == search_lower_bound` check above. */
                    .max(search_lower_bound);
            }
            let Some(new_pos) = pos.checked_sub(if has_end_sig {
                size_of_val(&ZIP64_CENTRAL_DIRECTORY_END_SIGNATURE) as u64
            } else {
                1
            }) else {
                break;
            };
            pos = new_pos;
        }

        if results.is_empty() {
            Err(ZipError::InvalidArchive(
                "Could not find ZIP64 central directory end",
            ))
        } else {
            Ok(results)
        }
    }

    pub fn block(self) -> Zip64CDEBlock {
        let Self {
            version_made_by,
            version_needed_to_extract,
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
        } = self;
        Zip64CDEBlock {
            magic: Zip64CDEBlock::MAGIC,
            /* currently unused */
            record_size: 44,
            version_made_by,
            version_needed_to_extract,
            disk_number,
            disk_with_central_directory,
            number_of_files_on_this_disk,
            number_of_files,
            central_directory_size,
            central_directory_offset,
        }
    }

    pub fn write<T: Write>(self, writer: &mut T) -> ZipResult<()> {
        self.block().write(writer)
    }
}

pub(crate) fn is_dir(filename: &str) -> bool {
    filename
        .chars()
        .next_back()
        .map_or(false, |c| c == '/' || c == '\\')
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Cursor;

    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
    #[repr(packed)]
    pub struct TestBlock {
        magic: Magic,
        pub file_name_length: u16,
    }

    impl FixedSizeBlock for TestBlock {
        const MAGIC: Magic = Magic::literal(0x01111);

        fn magic(self) -> Magic {
            self.magic
        }

        const WRONG_MAGIC_ERROR: ZipError = ZipError::InvalidArchive("unreachable");

        to_and_from_le![(magic, Magic), (file_name_length, u16)];
    }

    /// Demonstrate that a block object can be safely written to memory and deserialized back out.
    #[test]
    fn block_serde() {
        let block = TestBlock {
            magic: TestBlock::MAGIC,
            file_name_length: 3,
        };
        let mut c = Cursor::new(Vec::new());
        block.write(&mut c).unwrap();
        c.set_position(0);
        let block2 = TestBlock::parse(&mut c).unwrap();
        assert_eq!(block, block2);
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn zero_length_zip() {
        use super::CentralDirectoryEnd;
        use std::io;

        let v = vec![];
        let cde = CentralDirectoryEnd::find_and_parse(&mut io::Cursor::new(&v));
        assert!(cde.is_err());
    }

    #[test]
    fn invalid_cde_too_small() {
        use super::CentralDirectoryEnd;
        use std::io;

        // This is a valid CDE that _just_ fits (though there's nothing in front of it, so the offsets are wrong)
        let v = vec![
            0x50, 0x4b, 0x05, 0x06, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x03, 0x00, 0xe5, 0x00,
            0x00, 0x00, 0xd3, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let cde = CentralDirectoryEnd::find_and_parse(&mut io::Cursor::new(&v));
        assert!(cde.is_ok()); // This is ok, the offsets are checked elsewhere.

        // This is the same except the CDE is truncated by 4 bytes
        let cde = CentralDirectoryEnd::find_and_parse(&mut io::Cursor::new(&v[0..v.len() - 4]));
        assert!(cde.is_err());
    }

    #[test]
    fn invalid_cde_missing() {
        use super::CentralDirectoryEnd;
        use std::io;

        let v = [0; 70000]; // something larger than 65536 + CDE size
        let cde = CentralDirectoryEnd::find_and_parse(&mut io::Cursor::new(&v));
        assert!(cde.is_err());

        let v = [0; 256]; // something smaller than 65536 but larger CDE size
        let cde = CentralDirectoryEnd::find_and_parse(&mut io::Cursor::new(&v));
        assert!(cde.is_err());
    }

    fn zip64_cde_search(cde_start_pos: usize, total_size: usize) -> u64 {
        use super::Zip64CentralDirectoryEnd;
        use std::io;

        // 56 byte zip64 Central Directory End (extracted manually from tests/data/zip64_demo.zip)
        let cde64 = vec![
            0x50, 0x4b, 0x06, 0x06, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1e, 0x03,
            0x2d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2f, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let mut haystack = vec![0u8; cde_start_pos];
        haystack.extend_from_slice(&cde64);
        haystack.resize(total_size, 0u8);
        let results = Zip64CentralDirectoryEnd::find_and_parse(
            &mut io::Cursor::new(&haystack),
            0,
            haystack.len() as u64 - 56,
        )
        .expect("find_and_parse");
        let (_, offset) = results
            .into_iter()
            .max_by_key(|(_, offset)| *offset)
            .unwrap();
        offset
    }

    #[test]
    fn zip64_cde_search_less_than_chunk_at_start() {
        assert_eq!(0, zip64_cde_search(0, 100));
    }

    #[test]
    fn zip64_cde_search_less_than_chunk_in_middle() {
        assert_eq!(100, zip64_cde_search(100, 300));
    }

    #[test]
    fn zip64_cde_search_less_than_chunk_at_end() {
        assert_eq!(100, zip64_cde_search(100, 156));
    }

    #[test]
    fn zip64_cde_search_more_than_chunk_at_chunk_start() {
        assert_eq!(4096, zip64_cde_search(4096, 4200));
    }

    #[test]
    fn zip64_cde_search_more_than_chunk_mid_chunk() {
        assert_eq!(5000, zip64_cde_search(5000, 9000));
    }

    #[test]
    fn zip64_cde_search_more_than_chunk_at_chunk_end() {
        assert_eq!(8192 - 56, zip64_cde_search(8192 - 56, 8192));
    }

    #[test]
    fn zip64_cde_search_more_than_chunk_straddling_chunk_end() {
        assert_eq!(4096 - 30, zip64_cde_search(4096 - 30, 4200));
    }
}

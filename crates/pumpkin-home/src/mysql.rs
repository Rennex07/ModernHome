//! A MySQL and MariaDB client, small enough to read in one sitting.
//!
//! ## Why this exists
//!
//! The tested `mysql` 28.0.4 driver fails to build for WASIp3, including its
//! `getrandom` 0.3 dependency. `mysql_wasi` uses WasmEdge-specific sockets
//! rather than Wasmtime's standard WASI sockets.
//!
//! So the protocol is implemented here, over `std::net::TcpStream`, which the
//! WASI sockets in the host do provide.
//!
//! ## What is implemented
//!
//! * The version 10 handshake, with the server's capabilities read from the
//!   packet rather than assumed.
//! * `mysql_native_password`, which is what MariaDB uses by default and what
//!   MySQL uses when `default_authentication_plugin` says so.
//! * `caching_sha2_password`, up to the point where the server asks for the
//!   password over a secure channel. The fast path - the server has the
//!   password cached - works; the full path needs TLS or an RSA key exchange,
//!   neither of which is implemented here, and is refused with a message that
//!   says so.
//! * `COM_QUERY` with the text protocol: `SELECT` answers rows, everything else
//!   answers `OK`.
//!
//! ## What is not implemented
//!
//! * TLS. Queries and results are unencrypted. Authentication sends a
//!   challenge response, never the plaintext password.
//! * Prepared statements. Text values use UTF-8 hex literals instead.
//! * Compression, multi-statement queries, and the binary protocol.
//!
//! ## Blocking
//!
//! The client blocks. A query in a plugin callback blocks the thread the
//! callback runs on, so a slow database is a slow server. The store keeps the
//! homes in memory and writes only the players whose homes changed, which
//! keeps the number of queries small.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use sha1::Digest;
use sha2::Sha256;

/// The client capabilities this implementation asks for.
///
/// `CLIENT_PROTOCOL_41` is the version 10 protocol. `CLIENT_PLUGIN_AUTH` is
/// what tells the server the client can answer with a named authentication
/// plugin, and `CLIENT_SECURE_CONNECTION` is the SHA1 based password hashing.
/// `CLIENT_CONNECT_WITH_DB` is what lets the database be chosen in the
/// handshake rather than with a `USE` statement afterwards.
const CAPABILITIES: u32 = 0x0000_0200 // CLIENT_PROTOCOL_41
    | 0x0000_8000 // CLIENT_SECURE_CONNECTION
    | 0x0008_0000 // CLIENT_PLUGIN_AUTH
    | 0x0000_0008; // CLIENT_CONNECT_WITH_DB

/// The character set this client asks for: `utf8mb4_general_ci`.
const CHARSET: u8 = 45;

/// The largest packet this client reads or writes.
const MAX_PACKET: usize = 16 * 1024 * 1024 - 1;

/// A MySQL protocol error.
#[derive(Debug)]
pub enum Error {
    /// The server could not be reached.
    Io(io::Error),
    /// The server sent something this client does not understand.
    Protocol(String),
    /// The server refused the connection.
    Server {
        /// The server's error code.
        code: u16,
        /// The server's message.
        message: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(error) => write!(formatter, "{error}"),
            Error::Protocol(message) => write!(formatter, "{message}"),
            Error::Server { code, message } => {
                write!(
                    formatter,
                    "the server refused the query ({code}): {message}"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

/// One row of a result set, as the text protocol carries it.
///
/// Every column is a string, because that is what the text protocol sends;
/// `NULL` is [`None`].
pub type Row = Vec<Option<String>>;

/// A connection to a MySQL or MariaDB server.
#[derive(Debug)]
pub struct Client {
    /// The connection itself.
    stream: TcpStream,
    /// The sequence number of the next packet, which every packet has to carry.
    sequence: u8,
}

impl Client {
    /// Connects to `host`:`port` and authenticates as `user`.
    ///
    /// The connection is closed when it is dropped.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached, when it refuses the
    /// credentials, or when it asks for an authentication this client does not
    /// implement.
    pub fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        database: &str,
    ) -> Result<Self, Error> {
        let address = (host, port)
            .to_socket_addrs()
            .map_err(|error| Error::Protocol(format!("{host}:{port} does not resolve: {error}")))?
            .next()
            .ok_or_else(|| Error::Protocol(format!("{host}:{port} does not resolve")))?;

        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.set_nodelay(true)?;

        let mut client = Client {
            stream,
            sequence: 0,
        };
        client.handshake(user, password, database)?;
        Ok(client)
    }

    /// Reads the server's greeting and answers it.
    fn handshake(&mut self, user: &str, password: &str, database: &str) -> Result<(), Error> {
        if user.contains('\0') || database.contains('\0') {
            return Err(Error::Protocol(
                "user and database must not contain NUL".into(),
            ));
        }
        let greeting = self.read_packet()?;
        if greeting.first() == Some(&0xff) {
            return Err(server_error(&greeting));
        }
        let handshake = Handshake::parse(&greeting)?;
        if handshake.capabilities & CAPABILITIES != CAPABILITIES {
            return Err(Error::Protocol(
                "server lacks required protocol capabilities".into(),
            ));
        }

        // The server's plugin is what decides the answer. The default is the
        // SHA1 based one, which is what every MariaDB and every MySQL that
        // still uses `mysql_native_password` asks for.
        let plugin = handshake.auth_plugin.clone();
        let answer = auth_response(&plugin, password, &handshake.nonce)?;

        let mut response = Vec::new();
        response.extend_from_slice(&CAPABILITIES.to_le_bytes());
        response.extend_from_slice(&(MAX_PACKET as u32).to_le_bytes());
        response.push(CHARSET);
        response.extend_from_slice(&[0u8; 23]);
        response.extend_from_slice(user.as_bytes());
        response.push(0);
        response.push(answer.len() as u8);
        response.extend_from_slice(&answer);
        response.extend_from_slice(database.as_bytes());
        response.push(0);
        response.extend_from_slice(plugin.as_bytes());
        response.push(0);
        self.write_packet(&response)?;

        self.finish_handshake(password)
    }

    /// Reads the answer to the handshake and answers a switch if the server
    /// asks for one.
    fn finish_handshake(&mut self, password: &str) -> Result<(), Error> {
        for _ in 0..8 {
            let packet = self.read_packet()?;
            match packet.first() {
                Some(0x00) => return validate_ok(&packet),
                Some(0xff) => return Err(server_error(&packet)),
                Some(0xfe) => {
                    // An authentication switch: the server wants a different
                    // plugin from the one that was offered.
                    let mut cursor = 1;
                    let plugin = read_string(&packet, &mut cursor)?;
                    let new_nonce = packet.get(cursor..).ok_or_else(truncated)?;
                    let new_nonce = new_nonce.strip_suffix(&[0]).unwrap_or(new_nonce);
                    let answer = auth_response(&plugin, password, new_nonce)?;
                    self.write_packet(&answer)?;
                }
                Some(0x01) if packet.get(1) == Some(&0x03) => continue,
                Some(0x01) if packet.get(1) == Some(&0x04) => {
                    // `caching_sha2_password` full authentication, which needs a
                    // secure channel this client does not have.
                    return Err(Error::Protocol(
                        "the server asked for the password over a secure channel, which this \
                         client does not implement; TLS or a trusted RSA key is required"
                            .to_string(),
                    ));
                }
                _ => {
                    return Err(Error::Protocol(
                        "the server sent an answer this client does not understand".to_string(),
                    ));
                }
            }
        }
        Err(Error::Protocol("too many authentication exchanges".into()))
    }

    /// Runs a statement that answers no rows.
    ///
    /// # Errors
    ///
    /// Fails when the server refuses the statement or the connection breaks.
    pub fn execute(&mut self, sql: &str) -> Result<(), Error> {
        self.query(sql).map(|_| ())
    }

    /// Runs a statement and reads the rows it answers.
    ///
    /// # Errors
    ///
    /// Fails when the server refuses the statement, when the connection breaks,
    /// or when the answer is not a result set.
    pub fn query(&mut self, sql: &str) -> Result<Vec<Row>, Error> {
        self.sequence = 0;
        let mut request = Vec::with_capacity(sql.len() + 1);
        request.push(0x03); // COM_QUERY
        request.extend_from_slice(sql.as_bytes());
        self.write_packet(&request)?;

        let packet = self.read_packet()?;
        match packet.first() {
            Some(0x00) => {
                validate_ok(&packet)?;
                Ok(Vec::new())
            }
            Some(0xff) => Err(server_error(&packet)),
            Some(0xfb) => Err(Error::Protocol("LOCAL INFILE is not supported".to_string())),
            _ => {
                let mut cursor = 0;
                let columns = read_lenenc(&packet, &mut cursor)?;
                if columns == 0 || columns > 256 || cursor != packet.len() {
                    return Err(Error::Protocol("invalid result column count".into()));
                }
                let columns = columns as usize;
                // The column definitions, which this client does not need: the
                // text protocol says what each column is, but the rows carry
                // the same order.
                for _ in 0..columns {
                    let definition = self.read_packet()?;
                    if definition.first() == Some(&0xff) {
                        return Err(server_error(&definition));
                    }
                }
                // The end of the column definitions. This client does not ask
                // for `CLIENT_DEPRECATE_EOF`, so the server sends an EOF
                // packet here rather than an OK packet.
                validate_eof(&self.read_packet()?)?;

                let mut rows = Vec::new();
                let mut result_bytes = 0usize;
                loop {
                    let packet = self.read_packet()?;
                    result_bytes += packet.len();
                    if result_bytes > 64 * 1024 * 1024 || rows.len() >= 100_000 {
                        return Err(Error::Protocol("database result exceeds size limit".into()));
                    }
                    match packet.first() {
                        Some(0xff) => return Err(server_error(&packet)),
                        Some(0xfe) if packet.len() < 9 => {
                            validate_eof(&packet)?;
                            break;
                        }
                        _ => {
                            let mut cursor = 0;
                            let mut row = Vec::with_capacity(columns);
                            for _ in 0..columns {
                                if cursor < packet.len() && packet[cursor] == 0xfb {
                                    cursor += 1;
                                    row.push(None);
                                    continue;
                                }
                                row.push(Some(read_lenenc_string(&packet, &mut cursor)?));
                            }
                            if cursor != packet.len() {
                                return Err(Error::Protocol(
                                    "trailing data in database row".into(),
                                ));
                            }
                            rows.push(row);
                        }
                    }
                }
                Ok(rows)
            }
        }
    }

    /// Reads one packet, joining the parts of a packet that is too long for
    /// one read.
    fn read_packet(&mut self) -> Result<Vec<u8>, Error> {
        let mut payload = Vec::new();
        loop {
            let mut header = [0u8; 4];
            self.stream.read_exact(&mut header)?;
            let length = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
            if payload.len() + length > MAX_PACKET {
                return Err(Error::Protocol("database packet exceeds size limit".into()));
            }
            if header[3] != self.sequence {
                return Err(Error::Protocol("database packet sequence mismatch".into()));
            }
            self.sequence = self.sequence.wrapping_add(1);
            let mut chunk = vec![0u8; length];
            self.stream.read_exact(&mut chunk)?;
            payload.extend_from_slice(&chunk);
            if length < MAX_PACKET {
                break;
            }
        }
        Ok(payload)
    }

    /// Writes one packet.
    fn write_packet(&mut self, payload: &[u8]) -> Result<(), Error> {
        if payload.len() >= MAX_PACKET {
            return Err(Error::Protocol(
                "database request exceeds size limit".into(),
            ));
        }
        let mut header = [0u8; 4];
        header[0] = (payload.len() as u32 & 0xff) as u8;
        header[1] = (payload.len() as u32 >> 8) as u8;
        header[2] = (payload.len() as u32 >> 16) as u8;
        header[3] = self.sequence;
        self.stream.write_all(&header)?;
        self.stream.write_all(payload)?;
        self.stream.flush()?;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }
}

/// The server's greeting.
struct Handshake {
    capabilities: u32,
    /// The authentication plugin the server wants.
    auth_plugin: String,
    /// The nonce the server's answer is built from.
    nonce: Vec<u8>,
}

impl Handshake {
    /// Reads the version 10 greeting.
    fn parse(packet: &[u8]) -> Result<Self, Error> {
        if packet.first() != Some(&10) {
            return Err(Error::Protocol(
                "the server did not send a version 10 greeting".to_string(),
            ));
        }
        let mut cursor = 1;
        // The server version, which is not used.
        read_string(packet, &mut cursor)?;
        if packet.len().saturating_sub(cursor) < 31 {
            return Err(truncated());
        }
        cursor += 4; // The connection id.
        let mut nonce = Vec::new();
        nonce.extend_from_slice(&packet[cursor..cursor + 8]);
        cursor += 8;
        cursor += 1; // The filler.
        let low = u16::from_le_bytes([packet[cursor], packet[cursor + 1]]) as u32;
        cursor += 2;
        cursor += 1; // The character set.
        cursor += 2; // The status flags.
        let high = u16::from_le_bytes([packet[cursor], packet[cursor + 1]]) as u32;
        cursor += 2;
        let capabilities = low | (high << 16);
        let auth_data_length = packet[cursor] as usize;
        cursor += 1;
        cursor += 10; // The reserved bytes.

        if capabilities & 0x0008_0000 != 0 {
            // The rest of the nonce.
            let length = auth_data_length.saturating_sub(8).max(13);
            let end = cursor
                .checked_add(length)
                .filter(|end| *end <= packet.len())
                .ok_or_else(truncated)?;
            let mut rest = &packet[cursor..end];
            if let Some(stripped) = rest.strip_suffix(&[0]) {
                rest = stripped;
            }
            nonce.extend_from_slice(rest);
            cursor = end;

            // Some servers put a null byte of their own after the
            // authentication data, before the plugin name.
            while cursor < packet.len() && packet[cursor] == 0 {
                cursor += 1;
            }

            // The authentication plugin name.
            let plugin = read_string(packet, &mut cursor)?;
            return Ok(Handshake {
                capabilities,
                auth_plugin: plugin,
                nonce,
            });
        }

        // Without `CLIENT_PLUGIN_AUTH` the rest of the nonce is eight bytes
        // and the plugin is the SHA1 one.
        let end = (cursor + 12).min(packet.len());
        let mut rest = &packet[cursor..end];
        if let Some(stripped) = rest.strip_suffix(&[0]) {
            rest = stripped;
        }
        nonce.extend_from_slice(rest);
        Ok(Handshake {
            capabilities,
            auth_plugin: "mysql_native_password".to_string(),
            nonce,
        })
    }
}

/// The answer to the server's greeting, for the plugin it asked for.
///
/// # Errors
///
/// Fails when the server asked for a plugin this client does not implement.
fn auth_response(plugin: &str, password: &str, nonce: &[u8]) -> Result<Vec<u8>, Error> {
    match plugin {
        "mysql_native_password" => Ok(native_password(password, nonce)),
        "caching_sha2_password" => {
            // The fast path: the SHA256 of the password, XORed with the
            // nonce. The server accepts this when it has the password cached,
            // and otherwise asks for a secure channel, which is refused with a
            // message of its own.
            Ok(caching_sha2_fast(password, nonce))
        }
        other => Err(Error::Protocol(format!(
            "the server asked for the `{other}` authentication plugin, which this client does \
             not implement"
        ))),
    }
}

/// The SHA1 based answer: `SHA1(password) XOR SHA1(nonce + SHA1(SHA1(password)))`.
fn native_password(password: &str, nonce: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let first = sha1::Sha1::digest(password.as_bytes());
    let second = sha1::Sha1::digest(first);
    let mut seeded = nonce.to_vec();
    seeded.extend_from_slice(&second);
    let hash = sha1::Sha1::digest(seeded);
    first
        .iter()
        .zip(hash.iter())
        .map(|(left, right)| left ^ right)
        .collect()
}

/// The `caching_sha2_password` fast answer.
fn caching_sha2_fast(password: &str, nonce: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let digest = Sha256::digest(password.as_bytes());
    let second = Sha256::digest(digest);
    let mut seeded = second.to_vec();
    seeded.extend_from_slice(nonce);
    let hash = Sha256::digest(seeded);
    digest
        .iter()
        .zip(hash.iter())
        .map(|(left, right)| left ^ right)
        .collect()
}

/// The server's error packet, as an error.
fn server_error(packet: &[u8]) -> Error {
    if packet.len() < 3 {
        return truncated();
    }
    let mut cursor = 1;
    let code = u16::from_le_bytes([packet[cursor], packet[cursor + 1]]);
    cursor += 2;
    if packet.get(cursor) == Some(&0x23) {
        // The SQL state marker.
        cursor += 1;
        cursor += 5;
    }
    let message = String::from_utf8_lossy(&packet[cursor.min(packet.len())..]).to_string();
    Error::Server { code, message }
}

/// Reads a null terminated string.
fn read_string(packet: &[u8], cursor: &mut usize) -> Result<String, Error> {
    let start = *cursor;
    while *cursor < packet.len() && packet[*cursor] != 0 {
        *cursor += 1;
    }
    let bytes = packet.get(start..*cursor).ok_or_else(truncated)?;
    if *cursor >= packet.len() {
        return Err(truncated());
    }
    let text =
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::Protocol("invalid UTF-8".into()))?;
    *cursor += 1;
    Ok(text)
}

/// Reads a length encoded integer.
fn read_lenenc(packet: &[u8], cursor: &mut usize) -> Result<u64, Error> {
    let first = *packet
        .get(*cursor)
        .ok_or_else(|| Error::Protocol("a length ended early".to_string()))?;
    *cursor += 1;
    let count = match first {
        0..=0xfa => return Ok(u64::from(first)),
        0xfc => 2,
        0xfd => 3,
        0xfe => 8,
        _ => return Err(Error::Protocol("invalid length marker".into())),
    };
    let end = cursor.checked_add(count).ok_or_else(truncated)?;
    let source = packet.get(*cursor..end).ok_or_else(truncated)?;
    let mut bytes = [0u8; 8];
    bytes[..count].copy_from_slice(source);
    *cursor = end;
    Ok(u64::from_le_bytes(bytes))
}

/// Reads a length encoded string.
fn read_lenenc_string(packet: &[u8], cursor: &mut usize) -> Result<String, Error> {
    let length = usize::try_from(read_lenenc(packet, cursor)?).map_err(|_| truncated())?;
    let end = cursor.checked_add(length).ok_or_else(truncated)?;
    let bytes = packet.get(*cursor..end).ok_or_else(truncated)?;
    let text =
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::Protocol("invalid UTF-8".into()))?;
    *cursor = end;
    Ok(text)
}

/// Encodes UTF-8 as a SQL hex literal, independent of backslash SQL modes.
#[must_use]
pub fn quote(value: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::from("CONVERT(X'");
    for byte in value.as_bytes() {
        write!(encoded, "{byte:02x}").expect("writing to a String");
    }
    encoded.push_str("' USING utf8mb4)");
    encoded
}

fn truncated() -> Error {
    Error::Protocol("truncated database packet".into())
}

/// Confirm completion before treating a statement (especially COMMIT) as saved.
fn validate_ok(packet: &[u8]) -> Result<(), Error> {
    if packet.first() != Some(&0x00) {
        return Err(Error::Protocol("expected database OK packet".into()));
    }
    let mut cursor = 1;
    read_lenenc(packet, &mut cursor)?; // affected rows
    read_lenenc(packet, &mut cursor)?; // last insert id
    let status = packet.get(cursor..cursor + 4).ok_or_else(truncated)?;
    validate_status(u16::from_le_bytes([status[0], status[1]]))
}

fn validate_eof(packet: &[u8]) -> Result<(), Error> {
    if packet.first() == Some(&0xff) {
        return Err(server_error(packet));
    }
    if packet.first() != Some(&0xfe) || packet.len() != 5 {
        return Err(Error::Protocol("expected database EOF packet".into()));
    }
    validate_status(u16::from_le_bytes([packet[3], packet[4]]))
}

fn validate_status(status: u16) -> Result<(), Error> {
    if status & 0x0008 != 0 {
        return Err(Error::Protocol(
            "multiple database results are not supported".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    std::thread_local! {
        static SEQUENCE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    }

    /// The nonce the mock server sends.
    ///
    /// Twenty bytes: eight in the first part of the authentication data and
    /// twelve in the second, which is what the length byte in the greeting
    /// says.
    pub const NONCE: &[u8] = b"12345678abcdefghijkl";

    /// A server that answers the handshake and then answers every query.
    ///
    /// It is the smallest thing that speaks enough of the protocol for the
    /// client to be tested against: a greeting, the authentication, and a
    /// result set for every `SELECT`. Every statement it is sent is sent on
    /// the channel, so a test can see what the client asked for.
    pub fn mock_server(rows: Vec<Row>) -> (String, u16, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the mock server binds");
        let address = listener
            .local_addr()
            .expect("the mock server has an address");
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the client connects");
            let mut packet = Vec::new();
            packet.push(10); // The protocol version.
            packet.extend_from_slice(b"8.0.0-mock\0");
            packet.extend_from_slice(&1u32.to_le_bytes()); // The connection id.
            packet.extend_from_slice(&NONCE[..8]);
            packet.push(0); // The filler.
            let capabilities = 0x0000_0200 // CLIENT_PROTOCOL_41
                | 0x0000_8000 // CLIENT_SECURE_CONNECTION
                | 0x0008_0000 // CLIENT_PLUGIN_AUTH
                | 0x0000_0008; // CLIENT_CONNECT_WITH_DB
            packet.extend_from_slice(&(capabilities as u16).to_le_bytes());
            packet.push(33); // The character set.
            packet.extend_from_slice(&2u16.to_le_bytes()); // The status flags.
            packet.extend_from_slice(&((capabilities >> 16) as u16).to_le_bytes());
            packet.push(21); // The length of the authentication data.
            packet.extend_from_slice(&[0u8; 10]);
            packet.extend_from_slice(&NONCE[8..]);
            packet.push(0);
            packet.extend_from_slice(b"mysql_native_password\0");
            write_packet(&mut stream, &packet);

            // The handshake response, which the test reads rather than checks.
            let _ = read_packet(&mut stream);

            // The answer: OK.
            write_packet(&mut stream, &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]);

            while let Ok(request) = read_packet(&mut stream) {
                if request.first() != Some(&0x03) {
                    break;
                }
                let sql = String::from_utf8_lossy(&request[1..]).to_string();
                let _ = sender.send(sql.clone());

                if sql.starts_with("SELECT") {
                    let engine_rows = vec![vec![Some("InnoDB".to_string())]];
                    let result = if sql.starts_with("SELECT ENGINE FROM information_schema.TABLES")
                    {
                        &engine_rows
                    } else {
                        &rows
                    };
                    let columns = result.first().map_or(0, |row| row.len());
                    write_packet(&mut stream, &[columns as u8]);
                    for _ in 0..columns {
                        write_packet(&mut stream, b"column");
                    }
                    write_packet(&mut stream, &[0xfe, 0x00, 0x00, 0x02, 0x00]);
                    for row in result {
                        let mut packet = Vec::new();
                        for value in row {
                            match value {
                                Some(value) => {
                                    packet.push(value.len() as u8);
                                    packet.extend_from_slice(value.as_bytes());
                                }
                                None => packet.push(0xfb),
                            }
                        }
                        write_packet(&mut stream, &packet);
                    }
                    write_packet(&mut stream, &[0xfe, 0x00, 0x00, 0x02, 0x00]);
                } else {
                    write_packet(&mut stream, &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]);
                }
            }
        });

        (address.ip().to_string(), address.port(), receiver)
    }

    /// Writes one packet.
    pub fn write_packet(stream: &mut TcpStream, payload: &[u8]) {
        let mut header = [0u8; 4];
        header[0] = payload.len() as u8;
        header[1] = (payload.len() >> 8) as u8;
        header[2] = (payload.len() >> 16) as u8;
        header[3] = SEQUENCE.with(|s| {
            let next = s.get();
            s.set(next.wrapping_add(1));
            next
        });
        stream.write_all(&header).expect("the packet is written");
        stream.write_all(payload).expect("the packet is written");
    }

    /// Reads one packet.
    pub fn read_packet(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
        let mut header = [0u8; 4];
        stream.read_exact(&mut header)?;
        SEQUENCE.with(|s| s.set(header[3].wrapping_add(1)));
        let length = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload)?;
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mysql::testing::{NONCE, mock_server, write_packet};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn a_query_answers_the_rows_the_server_sent() {
        let rows = vec![vec![
            Some("uuid-a".to_string()),
            Some("1".to_string()),
            Some("world".to_string()),
            Some("minecraft:overworld".to_string()),
            Some("1.5".to_string()),
            Some("64".to_string()),
            Some("-3.25".to_string()),
            Some("90".to_string()),
            Some("0".to_string()),
            Some("base".to_string()),
            Some("server-1".to_string()),
        ]];
        let (host, port, statements) = mock_server(rows);

        let mut client = Client::connect(&host, port, "user", "secret", "database")
            .expect("the client connects");
        let answer = client
            .query("SELECT uuid FROM homes")
            .expect("the query answers");

        assert_eq!(answer.len(), 1);
        assert_eq!(answer[0][0].as_deref(), Some("uuid-a"));
        assert_eq!(
            statements.recv().expect("the query is seen"),
            "SELECT uuid FROM homes"
        );
    }

    #[test]
    fn a_statement_that_answers_no_rows_is_not_an_error() {
        let (host, port, statements) = mock_server(Vec::new());

        let mut client = Client::connect(&host, port, "user", "secret", "database")
            .expect("the client connects");
        client
            .execute("DELETE FROM homes WHERE uuid = 'uuid-a'")
            .expect("the statement runs");

        assert_eq!(
            statements.recv().expect("the statement is seen"),
            "DELETE FROM homes WHERE uuid = 'uuid-a'"
        );
    }

    #[test]
    fn the_password_answer_is_the_sha1_one() {
        // The formula is the server's: `SHA1(password) XOR SHA1(nonce + SHA1(SHA1(password)))`.
        let expected = native_password("secret", NONCE);
        let first = sha1::Sha1::digest(b"secret");
        let second = sha1::Sha1::digest(first);
        let mut seeded = NONCE.to_vec();
        seeded.extend_from_slice(&second);
        let hash = sha1::Sha1::digest(seeded);
        let expected_by_hand: Vec<u8> = first
            .iter()
            .zip(hash.iter())
            .map(|(left, right)| left ^ right)
            .collect();
        assert_eq!(expected, expected_by_hand);
        assert_eq!(expected.len(), 20);
    }

    #[test]
    fn text_literals_do_not_depend_on_backslash_escaping() {
        assert_eq!(quote("it's"), "CONVERT(X'69742773' USING utf8mb4)");
        assert_eq!(quote("\\\0"), "CONVERT(X'5c00' USING utf8mb4)");
        assert_eq!(quote("雪"), "CONVERT(X'e99baa' USING utf8mb4)");
    }

    #[test]
    fn caching_sha2_matches_the_protocol_reference_vector() {
        let expected = [
            0x7f, 0x7e, 0x38, 0x78, 0xfe, 0x48, 0x8e, 0xd4, 0x50, 0x81, 0x45, 0x5d, 0x0d, 0x6e,
            0x89, 0xe2, 0x6d, 0x4e, 0xf8, 0x6e, 0xf6, 0xc6, 0xae, 0xed, 0x02, 0x39, 0x7b, 0x6f,
            0xe8, 0xad, 0x54, 0x22,
        ];
        assert_eq!(caching_sha2_fast("secret", NONCE), expected);
    }

    #[test]
    fn truncated_packets_are_errors_not_panics() {
        for packet in [vec![], vec![10], vec![10, 0], vec![10, 0, 1, 2]] {
            assert!(Handshake::parse(&packet).is_err());
        }
        for prefix in [0xfc, 0xfd, 0xfe, 0xff, 0xfb] {
            assert!(read_lenenc(&[prefix], &mut 0).is_err());
        }
        assert!(read_lenenc_string(&[5, b'a'], &mut 0).is_err());
        assert!(matches!(server_error(&[0xff]), Error::Protocol(_)));
    }

    #[test]
    fn statement_completion_requires_a_complete_ok_packet() {
        let ok = [0, 0, 0, 2, 0, 0, 0];
        for length in 0..ok.len() {
            assert!(validate_ok(&ok[..length]).is_err());
        }
        assert!(validate_ok(&ok).is_ok());
        assert!(validate_ok(&[0, 0, 0, 8, 0, 0, 0]).is_err());
    }

    #[test]
    fn result_completion_requires_a_complete_eof_packet() {
        let eof = [0xfe, 0, 0, 2, 0];
        for length in 0..eof.len() {
            assert!(validate_eof(&eof[..length]).is_err());
        }
        assert!(validate_eof(&eof).is_ok());
        assert!(validate_eof(&[0xfe, 0, 0, 8, 0]).is_err());
        assert!(matches!(
            validate_eof(&[0xff, 1, 0]),
            Err(Error::Server { .. })
        ));
    }

    #[test]
    fn a_server_that_answers_nonsense_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the mock server binds");
        let address = listener
            .local_addr()
            .expect("the mock server has an address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the client connects");
            write_packet(&mut stream, &[0xff, 0x10, 0x04]);
        });

        let error = Client::connect(&address.ip().to_string(), address.port(), "u", "p", "d")
            .expect_err("the greeting is refused");
        assert!(
            matches!(error, Error::Server { code: 1040, .. }),
            "the greeting preserves the server error"
        );
    }
}

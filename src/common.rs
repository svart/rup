use mio::Token;

pub const TOKEN_READ_SOCKET: Token = Token(0);
pub const TOKEN_SEND_TIMEOUT: Token = Token(1);

pub const DEFAULT_READ_RESPONSE_TIMEOUT: u64 = 1000;

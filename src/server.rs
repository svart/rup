use std::io::Result;
use mio::{Interest, Poll};
use mio::event::Source;

use crate::common::{TOKEN_READ_SOCKET};

pub fn setup_server_polling<T: Source>(socket: &mut T) -> Result<Poll> {
    let poller = Poll::new()?;
    poller.registry()
          .register(socket, TOKEN_READ_SOCKET, Interest::READABLE)?;
    Ok(poller)
}


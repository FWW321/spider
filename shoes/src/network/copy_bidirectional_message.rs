use futures::ready;
use tokio::io::ReadBuf;
use pin_project_lite::pin_project;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::time::Sleep;

use crate::network::async_stream::AsyncMessageStream;
use crate::utils::common::allocate_vec;

// Informed by https://stackoverflow.com/questions/14856639/udp-hole-punching-timeout
pub const DEFAULT_ASSOCIATION_TIMEOUT_SECS: u32 = 200;

struct CopyBuffer {
    read_done: bool,
    need_flush: bool,
    need_write_ping: bool,
    cache_length: usize,
    buf: Box<[u8]>,
    read_count: usize,
}

impl CopyBuffer {
    pub fn new(need_flush: bool) -> Self {
        Self {
            read_done: false,
            need_flush,
            need_write_ping: false,
            cache_length: 0,
            buf: allocate_vec(65535).into_boxed_slice(),
            read_count: 0,
        }
    }

    pub fn poll_copy<R, W>(
        &mut self,
        cx: &mut Context<'_>,
        mut reader: Pin<&mut R>,
        mut writer: Pin<&mut W>,
    ) -> Poll<io::Result<()>>
    where
        R: AsyncMessageStream + ?Sized,
        W: AsyncMessageStream + ?Sized,
    {
        // Check tokio's cooperative budget to prevent task starvation
        let coop = ready!(tokio::task::coop::poll_proceed(cx));

        loop {
            let mut did_read = false;
            let mut did_write = false;
            let mut read_pending = false;
            let mut write_pending = false;

            if !self.read_done && self.cache_length == 0 {
                let me = &mut *self;
                let mut buf = ReadBuf::new(&mut me.buf);
                match reader.as_mut().poll_read_message(cx, &mut buf) {
                    Poll::Ready(val) => {
                        val?;
                        let n = buf.filled().len();
                        if n == 0 {
                            self.read_done = true;
                        } else {
                            self.cache_length = n;
                            did_read = true;
                            self.read_count = self.read_count.wrapping_add(n);
                            coop.made_progress();
                        }
                    }
                    Poll::Pending => {
                        read_pending = true;
                    }
                }
            }

            if self.cache_length > 0 {
                let me = &mut *self;
                match writer
                    .as_mut()
                    .poll_write_message(cx, &me.buf[0..me.cache_length])
                {
                    Poll::Ready(val) => {
                        val?;
                        self.cache_length = 0;
                        self.need_flush = true;
                        // Don't bother writing ping, since we just wrote.
                        self.need_write_ping = false;
                        did_write = true;
                        coop.made_progress();
                    }
                    Poll::Pending => {
                        write_pending = true;
                    }
                }
            }

            if !write_pending && self.need_write_ping {
                match writer.as_mut().poll_write_ping(cx) {
                    Poll::Ready(val) => {
                        let written = val?;
                        self.need_write_ping = false;
                        if written {
                            self.need_flush = true;
                            coop.made_progress();
                        }
                    }
                    Poll::Pending => {
                        write_pending = true;
                    }
                }
            }

            if did_read && did_write && !read_pending && !write_pending {
                continue;
            }

            if self.need_flush {
                ready!(writer.as_mut().poll_flush_message(cx))?;
                self.need_flush = false;
                coop.made_progress();
                continue;
            }

            // If we've written all the data and we've seen EOF, finish the transfer.
            if self.read_done && self.cache_length == 0 {
                return Poll::Ready(Ok(()));
            }

            // Return Pending to prevent task starvation
            if read_pending || write_pending {
                return Poll::Pending;
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum TransferState {
    Running,
    ShuttingDown,
    Done,
}

pin_project! {
    struct CopyBidirectional<'a, A: ?Sized, B: ?Sized> {
        a: &'a mut A,
        b: &'a mut B,
        a_buf: CopyBuffer,
        b_buf: CopyBuffer,
        a_to_b: TransferState,
        b_to_a: TransferState,
        #[pin]
        sleep_future: Sleep,
        last_active: Instant,
    }
}

fn transfer_one_direction<A, B>(
    cx: &mut Context<'_>,
    state: &mut TransferState,
    buf: &mut CopyBuffer,
    r: &mut A,
    w: &mut B,
) -> Poll<io::Result<()>>
where
    A: AsyncMessageStream + ?Sized,
    B: AsyncMessageStream + ?Sized,
{
    let mut r = Pin::new(r);
    let mut w = Pin::new(w);

    loop {
        match state {
            TransferState::Running => {
                ready!(buf.poll_copy(cx, r.as_mut(), w.as_mut()))?;
                *state = TransferState::ShuttingDown;
            }
            TransferState::ShuttingDown => {
                ready!(w.as_mut().poll_shutdown_message(cx))?;
                *state = TransferState::Done;
            }
            TransferState::Done => return Poll::Ready(Ok(())),
        }
    }
}

impl<A, B> Future for CopyBidirectional<'_, A, B>
where
    A: AsyncMessageStream + ?Sized,
    B: AsyncMessageStream + ?Sized,
{
    type Output = io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        if this.sleep_future.as_mut().poll(cx).is_ready() {
            this.a_buf.need_write_ping = this.b.supports_ping();
            this.b_buf.need_write_ping = this.a.supports_ping();
            this.sleep_future
                .as_mut()
                .reset(tokio::time::Instant::now() + std::time::Duration::from_secs(60));
        }

        let a_count = this.a_buf.read_count;
        let b_count = this.b_buf.read_count;

        let a_to_b = transfer_one_direction(cx, this.a_to_b, this.a_buf, *this.a, *this.b);
        let b_to_a = transfer_one_direction(cx, this.b_to_a, this.b_buf, *this.b, *this.a);

        if this.a_buf.read_count != a_count || this.b_buf.read_count != b_count {
            *this.last_active = Instant::now();
        } else if this.last_active.elapsed().as_secs() >= DEFAULT_ASSOCIATION_TIMEOUT_SECS.into() {
            return Poll::Ready(Ok(()));
        }

        if a_to_b.is_ready() {
            return a_to_b;
        } else if b_to_a.is_ready() {
            return b_to_a;
        }

        Poll::Pending
    }
}

pub async fn copy_bidirectional_message<A, B>(
    a: &mut A,
    b: &mut B,
    a_initial_flush: bool,
    b_initial_flush: bool,
) -> Result<(), std::io::Error>
where
    A: AsyncMessageStream + ?Sized,
    B: AsyncMessageStream + ?Sized,
{
    // Unlike tcp copy_bidirectional, we always run a sleep future so that we can expire
    // connections.
    let sleep_future = tokio::time::sleep(std::time::Duration::from_secs(60));

    CopyBidirectional {
        a,
        b,
        a_buf: CopyBuffer::new(b_initial_flush),
        b_buf: CopyBuffer::new(a_initial_flush),
        a_to_b: TransferState::Running,
        b_to_a: TransferState::Running,
        sleep_future,
        last_active: Instant::now(),
    }
    .await
}
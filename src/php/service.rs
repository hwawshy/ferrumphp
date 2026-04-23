use crate::php::Job;
use hyper::body::Incoming;
use hyper::{Request, Response};
use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio_util::sync::PollSender;
use tower::Service;

#[derive(Clone)]
pub struct PhpService {
    sender: PollSender<Job>,
}

impl PhpService {
    pub fn new(sender: Sender<Job>) -> Self {
        Self {
            sender: PollSender::new(sender),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PhpError {
    ChannelClosed,
}

impl std::fmt::Display for PhpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PHP Error!")
    }
}

impl Error for PhpError {}

impl Service<Request<Incoming>> for PhpService {
    type Response = Response<String>;
    type Error = PhpError;
    type Future = PhpFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sender
            .poll_reserve(cx)
            .map_err(|_| PhpError::ChannelClosed)
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let (tx_resp, rx_resp) = channel::<Self::Response>(10);
        let job = Job {
            request: req,
            respond_to: tx_resp,
        };

        match self.sender.send_item(job) {
            Ok(()) => PhpFuture::Ok(rx_resp),
            Err(_) => PhpFuture::Err(PhpError::ChannelClosed),
        }
    }
}

pub enum PhpFuture {
    Ok(Receiver<Response<String>>),
    Err(PhpError),
}

impl Future for PhpFuture {
    type Output = Result<Response<String>, PhpError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this {
            PhpFuture::Err(e) => Poll::Ready(Err(*e)),
            PhpFuture::Ok(receiver) => match receiver.poll_recv(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Some(resp)) => Poll::Ready(Ok(resp)),
                Poll::Ready(None) => Poll::Ready(Err(PhpError::ChannelClosed)),
            },
        }
    }
}

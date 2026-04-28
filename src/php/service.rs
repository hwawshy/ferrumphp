use crate::php::Job;
use bytes::Bytes;
use futures_util::stream::{Map, StreamExt};
use http_body_util::StreamBody;
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response};
use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio_stream::wrappers::ReceiverStream;
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
    type Response = Response<StreamBody<PhpStream>>;
    type Error = PhpError;
    type Future = PhpFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sender
            .poll_reserve(cx)
            .map_err(|_| PhpError::ChannelClosed)
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let (tx_resp, rx_resp) = channel::<Bytes>(10);
        let job = Job {
            request: req,
            respond_to: tx_resp,
        };

        match self.sender.send_item(job) {
            Ok(()) => PhpFuture::Ok(Some(rx_resp)),
            Err(_) => PhpFuture::Err(PhpError::ChannelClosed),
        }
    }
}

pub enum PhpFuture {
    Ok(Option<Receiver<Bytes>>),
    Err(PhpError),
}

type PhpStream = Map<ReceiverStream<Bytes>, fn(Bytes) -> Result<Frame<Bytes>, PhpError>>;

impl Future for PhpFuture {
    type Output = Result<Response<StreamBody<PhpStream>>, PhpError>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            PhpFuture::Err(e) => Poll::Ready(Err(*e)),
            PhpFuture::Ok(rx) => {
                let stream: PhpStream =
                    ReceiverStream::new(rx.take().unwrap()).map(|chunk| Ok(Frame::data(chunk)));

                Poll::Ready(Ok(Response::new(StreamBody::new(stream))))
            }
        }
    }
}

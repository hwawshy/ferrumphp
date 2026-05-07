use crate::php::Job;
use bytes::Bytes;
use futures_util::stream::{Map, StreamExt};
use http_body_util::StreamBody;
use hyper::body::{Frame, Incoming};
use hyper::{HeaderMap, Request, Response};
use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::sync::oneshot;
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
        let (tx_header, rx_header) = oneshot::channel::<HeaderMap>();

        let job = Job {
            request: req,
            response_tx: tx_resp,
            header_tx: tx_header,
        };

        match self.sender.send_item(job) {
            Ok(()) => PhpFuture::Ok {
                header_rx: rx_header,
                response_rx: Some(rx_resp),
            },
            Err(_) => PhpFuture::Err(PhpError::ChannelClosed),
        }
    }
}

pub enum PhpFuture {
    Ok {
        response_rx: Option<Receiver<Bytes>>,
        header_rx: oneshot::Receiver<HeaderMap>,
    },
    Err(PhpError),
}

type PhpStream = Map<ReceiverStream<Bytes>, fn(Bytes) -> Result<Frame<Bytes>, PhpError>>;

impl Future for PhpFuture {
    type Output = Result<Response<StreamBody<PhpStream>>, PhpError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            PhpFuture::Err(e) => Poll::Ready(Err(*e)),
            PhpFuture::Ok {
                response_rx,
                header_rx,
            } => {
                tokio::pin!(header_rx);

                let header_map = match header_rx.poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(_)) => return Poll::Ready(Err(PhpError::ChannelClosed)),
                    Poll::Ready(Ok(header_map)) => header_map,
                };

                let (mut parts, _) = Response::<Bytes>::default().into_parts();
                parts.headers = header_map;

                let stream: PhpStream = ReceiverStream::new(response_rx.take().unwrap())
                    .map(|chunk| Ok(Frame::data(chunk)));

                Poll::Ready(Ok(Response::from_parts(parts, StreamBody::new(stream))))
            }
        }
    }
}

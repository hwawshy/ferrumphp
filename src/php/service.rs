use crate::php::Job;
use bytes::Bytes;
use futures_util::stream::{Map, StreamExt};
use http_body_util::combinators::Collect;
use http_body_util::{BodyExt, Collected, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::http::request::Parts;
use hyper::{HeaderMap, Request, Response};
use std::error::Error;
use std::fmt::Display;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::{PollSendError, PollSender};
use tower::Service;

#[derive(Clone)]
pub struct PhpService {
    sender: Option<PollSender<Job>>,
}

impl PhpService {
    pub fn new(sender: Sender<Job>) -> Self {
        Self {
            sender: Some(PollSender::new(sender)),
        }
    }
}

// todo look into using thiserror
#[derive(Clone, Copy, Debug)]
pub enum PhpError {
    Hyper,
    JobSendingFailed,
    HeaderChannelClosed,
}

impl Display for PhpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PHP Error!")
    }
}

impl Error for PhpError {}

impl From<hyper::Error> for PhpError {
    fn from(_: hyper::Error) -> Self {
        Self::Hyper
    }
}

impl From<PollSendError<Job>> for PhpError {
    fn from(_: PollSendError<Job>) -> Self {
        PhpError::JobSendingFailed
    }
}

impl From<RecvError> for PhpError {
    fn from(_: RecvError) -> Self {
        PhpError::HeaderChannelClosed
    }
}

impl Service<Request<Incoming>> for PhpService {
    type Response = Response<StreamBody<PhpStream>>;
    type Error = PhpError;
    type Future = PhpFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sender
            .as_mut()
            .expect("PollSender invalid")
            .poll_reserve(cx)
            .map_err(|_| PhpError::JobSendingFailed)
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let (parts, body) = req.into_parts();

        PhpFuture {
            sender: self.sender.take().expect("PollSender invalid"),
            collect: Some(body.collect()),
            parts: Some(parts),
            collected: None,
            response_rx: None,
            header_rx: None,
        }
    }
}

pub struct PhpFuture {
    sender: PollSender<Job>,
    collect: Option<Collect<Incoming>>,
    parts: Option<Parts>,
    collected: Option<Collected<Bytes>>,
    response_rx: Option<Receiver<Bytes>>,
    header_rx: Option<oneshot::Receiver<HeaderMap>>,
}

type PhpStream = Map<ReceiverStream<Bytes>, fn(Bytes) -> Result<Frame<Bytes>, PhpError>>;

impl Future for PhpFuture {
    type Output = Result<Response<StreamBody<PhpStream>>, PhpError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            match this.collect {
                Some(ref mut collect) => {
                    this.collected = Some(ready!(Pin::new(collect).poll(cx))?);
                    this.collect = None;
                }
                None => match this.header_rx {
                    None => {
                        let (response_tx, response_rx) = channel::<Bytes>(10);
                        let (header_tx, header_rx) = oneshot::channel::<HeaderMap>();

                        let request = Request::from_parts(
                            this.parts.take().unwrap(),
                            this.collected.take().unwrap().to_bytes(),
                        );
                        this.sender.send_item(Job {
                            request,
                            response_tx,
                            header_tx,
                        })?;

                        this.response_rx = Some(response_rx);
                        this.header_rx = Some(header_rx);
                    }
                    Some(ref mut header_rx) => {
                        let header_map = ready!(Pin::new(header_rx).poll(cx))?;

                        let (mut parts, _) = Response::<()>::default().into_parts();
                        parts.headers = header_map;

                        let stream: PhpStream =
                            ReceiverStream::new(this.response_rx.take().unwrap())
                                .map(|chunk| Ok(Frame::data(chunk)));

                        return Poll::Ready(Ok(Response::from_parts(
                            parts,
                            StreamBody::new(stream),
                        )));
                    }
                },
            }
        }
    }
}

use crate::php::Job;
use bytes::Bytes;
use futures_util::stream::{Map, StreamExt};
use http_body_util::StreamBody;
use hyper::body::{Body, Frame, Incoming};
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
    sender: PollSender<Job>,
}

impl PhpService {
    pub fn new(sender: Sender<Job>) -> Self {
        Self {
            sender: PollSender::new(sender),
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

impl<T> From<PollSendError<T>> for PhpError {
    fn from(_: PollSendError<T>) -> Self {
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
            .poll_reserve(cx)
            .map_err(|_| PhpError::JobSendingFailed)
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let (parts, body) = req.into_parts();

        let (request_body_tx, request_body_rx) = channel::<Bytes>(8); // @todo rethink this buffer
        let (response_body_tx, response_body_rx) = channel::<Bytes>(10); // @todo rethink this buffer
        let (response_header_tx, response_header_rx) = oneshot::channel::<HeaderMap>();

        let job = Job {
            request_head: parts,
            request_body_rx,
            response_header_tx,
            response_body_tx,
        };

        if let Err(_) = self.sender.send_item(job) {
            return PhpFuture::Err(PhpError::JobSendingFailed);
        }

        PhpFuture::Ok {
            request_body: Some(body),
            current_request_body_chunk: None,
            request_body_tx: PollSender::new(request_body_tx),
            response_body_rx: Some(response_body_rx),
            response_header_rx,
        }
    }
}

pub enum PhpFuture {
    Ok {
        request_body: Option<Incoming>,
        current_request_body_chunk: Option<Bytes>,
        request_body_tx: PollSender<Bytes>,
        response_header_rx: oneshot::Receiver<HeaderMap>,
        response_body_rx: Option<Receiver<Bytes>>,
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
                request_body,
                request_body_tx,
                current_request_body_chunk,
                response_header_rx,
                response_body_rx,
            } => {
                loop {
                    match request_body {
                        Some(body) => {
                            match current_request_body_chunk {
                                None => {
                                    let frame = ready!(Pin::new(body).poll_frame(cx));

                                    if let Some(frame) = frame {
                                        // ignore trailers
                                        if let Ok(data) = frame?.into_data() {
                                            *current_request_body_chunk = Some(data);
                                        }
                                    } else {
                                        // end of stream
                                        request_body.take();
                                        request_body_tx.close();
                                    }
                                }
                                Some(_) => {
                                    // stream body chunk
                                    ready!(Pin::new(&mut *request_body_tx).poll_reserve(cx))?;

                                    request_body_tx
                                        .send_item(current_request_body_chunk.take().unwrap())?
                                }
                            }
                        }
                        None => {
                            let header_map = ready!(Pin::new(response_header_rx).poll(cx))?;

                            // @todo status, version
                            let (mut parts, _) = Response::<()>::default().into_parts();
                            parts.headers = header_map;

                            let stream: PhpStream =
                                ReceiverStream::new(response_body_rx.take().unwrap())
                                    .map(|chunk| Ok(Frame::data(chunk)));

                            return Poll::Ready(Ok(Response::from_parts(
                                parts,
                                StreamBody::new(stream),
                            )));
                        }
                    }
                }
            }
        }
    }
}

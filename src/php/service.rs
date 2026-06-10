use crate::php::Job;
use bytes::Bytes;
use futures_util::stream::{Map, StreamExt};
use http_body_util::StreamBody;
use hyper::Error as HyperError;
use hyper::body::{Body, Frame, Incoming};
use hyper::http::response::Parts;
use hyper::{Request, Response};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use thiserror::Error;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::PollSender;
use tower::Service;

#[derive(Clone)]
pub struct PhpService {
    sender: PollSender<Job>,
    peer_addr: SocketAddr,
}

impl PhpService {
    pub fn new(sender: Sender<Job>, peer_addr: SocketAddr) -> Self {
        Self {
            sender: PollSender::new(sender),
            peer_addr,
        }
    }
}

#[derive(Error, Debug)]
pub enum PhpError {
    #[error("hyper transport error: {0}")]
    Hyper(#[from] HyperError),

    #[error("failed to enqueue job to worker")]
    JobChannelClosed,

    #[error("worker request body channel closed")]
    RequestBodyClosed,

    #[error("worker response header channel closed")]
    ResponseHeadClosed(#[from] RecvError),
}

impl Service<Request<Incoming>> for PhpService {
    type Response = Response<StreamBody<PhpStream>>;
    type Error = PhpError;
    type Future = PhpFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sender
            .poll_reserve(cx)
            .map_err(|_| PhpError::JobChannelClosed)
    }

    fn call(&mut self, mut req: Request<Incoming>) -> Self::Future {
        req.extensions_mut().insert(self.peer_addr);

        let (parts, body) = req.into_parts();

        let (response_body_tx, response_body_rx) = channel::<Bytes>(10); // @todo rethink this buffer
        let (response_head_tx, response_head_rx) = oneshot::channel::<Parts>();

        // Do we have a request body?
        let (job, fut) = if body.is_end_stream() {
            let job = Job {
                request_head: parts,
                request_body_rx: None,
                response_head_tx,
                response_body_tx,
            };

            let fut = PhpFuture::WaitingResponse {
                response_body_rx,
                response_head_rx,
            };

            (job, fut)
        } else {
            // This buffer plays a role in ensuring fairness cap per poll in PhpFuture
            let (request_body_tx, request_body_rx) = channel::<Bytes>(8); // @todo rethink this buffer

            let job = Job {
                request_head: parts,
                request_body_rx: Some(request_body_rx),
                response_head_tx,
                response_body_tx,
            };

            let fut = PhpFuture::StreamingRequest {
                request_body: body,
                request_body_tx: PollSender::new(request_body_tx),
                current_request_body_chunk: None,
                response_body_rx,
                response_head_rx,
            };

            (job, fut)
        };

        if let Err(_) = self.sender.send_item(job) {
            return PhpFuture::Err(PhpError::JobChannelClosed);
        };

        fut
    }
}

type PhpStream = Map<ReceiverStream<Bytes>, fn(Bytes) -> Result<Frame<Bytes>, PhpError>>;

pub enum PhpFuture {
    StreamingRequest {
        request_body: Incoming,
        request_body_tx: PollSender<Bytes>,
        current_request_body_chunk: Option<Bytes>,
        response_head_rx: oneshot::Receiver<Parts>,
        response_body_rx: Receiver<Bytes>,
    },
    WaitingResponse {
        response_head_rx: oneshot::Receiver<Parts>,
        response_body_rx: Receiver<Bytes>,
    },
    Err(PhpError),
    Done,
}

impl PhpFuture {
    fn transition_to_waiting_response(&mut self) {
        match std::mem::replace(self, PhpFuture::Done) {
            PhpFuture::StreamingRequest {
                response_head_rx,
                response_body_rx,
                ..
            } => {
                *self = PhpFuture::WaitingResponse {
                    response_head_rx,
                    response_body_rx,
                }
            }
            _ => unreachable!(),
        }
    }

    fn transition_to_done(&mut self) -> Receiver<Bytes> {
        match std::mem::replace(self, PhpFuture::Done) {
            PhpFuture::WaitingResponse {
                response_body_rx, ..
            } => response_body_rx,
            _ => unreachable!(),
        }
    }
}

impl Future for PhpFuture {
    type Output = Result<Response<StreamBody<PhpStream>>, PhpError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        loop {
            match this {
                PhpFuture::Done => panic!("PhpFuture polled after completion"),
                PhpFuture::Err(_) => match std::mem::replace(this, PhpFuture::Done) {
                    PhpFuture::Err(e) => return Poll::Ready(Err(e)),
                    _ => unreachable!(),
                },
                PhpFuture::StreamingRequest {
                    request_body,
                    request_body_tx,
                    current_request_body_chunk,
                    ..
                } => {
                    match current_request_body_chunk {
                        None => {
                            let frame = ready!(Pin::new(request_body).poll_frame(cx));

                            if let Some(frame) = frame {
                                // ignore trailers
                                if let Ok(data) = frame?.into_data() {
                                    *current_request_body_chunk = Some(data);
                                }
                            } else {
                                // end of stream
                                request_body_tx.close();
                                this.transition_to_waiting_response();
                            }
                        }
                        Some(_) => {
                            // stream body chunk
                            ready!(Pin::new(&mut *request_body_tx).poll_reserve(cx))
                                .map_err(|_| PhpError::RequestBodyClosed)?;

                            request_body_tx
                                .send_item(current_request_body_chunk.take().unwrap())
                                .map_err(|_| PhpError::RequestBodyClosed)?
                        }
                    }
                }
                PhpFuture::WaitingResponse {
                    response_head_rx, ..
                } => {
                    let parts = ready!(Pin::new(response_head_rx).poll(cx))
                        .map_err(|e| PhpError::ResponseHeadClosed(e))?;
                    let response_body_rx = this.transition_to_done();

                    let stream: PhpStream =
                        ReceiverStream::new(response_body_rx).map(|chunk| Ok(Frame::data(chunk)));

                    return Poll::Ready(Ok(Response::from_parts(parts, StreamBody::new(stream))));
                }
            }
        }
    }
}

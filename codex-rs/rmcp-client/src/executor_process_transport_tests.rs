use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use bytes::BytesMut;
use codex_exec_server::ExecOutputStream;
use codex_exec_server::ExecProcess;
use codex_exec_server::ExecProcessEventReceiver;
use codex_exec_server::ExecServerError;
use codex_exec_server::ProcessId;
use codex_exec_server::ProcessOutputChunk;
use codex_exec_server::ReadResponse;
use codex_exec_server::WriteResponse;
use codex_exec_server::WriteStatus;
use pretty_assertions::assert_eq;
use tokio::sync::watch;
use tokio::time::Duration;
use tokio::time::timeout;

use super::ExecutorProcessTransport;
use super::LineBuffer;
use super::LineTooLong;
use super::MAX_MCP_STDERR_LINE_BYTES;
use super::MAX_MCP_STDOUT_LINE_BYTES;

struct TestExecProcess {
    id: ProcessId,
    read_response: Mutex<ReadResponse>,
    terminated: AtomicBool,
}

impl TestExecProcess {
    fn new(read_response: ReadResponse) -> Self {
        Self {
            id: ProcessId::from("test-mcp"),
            read_response: Mutex::new(read_response),
            terminated: AtomicBool::new(false),
        }
    }

    fn empty() -> Self {
        Self::new(ReadResponse {
            chunks: Vec::new(),
            next_seq: 1,
            exited: false,
            exit_code: None,
            closed: false,
            failure: None,
        })
    }
}

#[async_trait]
impl ExecProcess for TestExecProcess {
    fn process_id(&self) -> &ProcessId {
        &self.id
    }

    fn subscribe_wake(&self) -> watch::Receiver<u64> {
        let (_tx, rx) = watch::channel(0);
        rx
    }

    fn subscribe_events(&self) -> ExecProcessEventReceiver {
        ExecProcessEventReceiver::empty()
    }

    async fn read(
        &self,
        _after_seq: Option<u64>,
        _max_bytes: Option<usize>,
        _wait_ms: Option<u64>,
    ) -> Result<ReadResponse, ExecServerError> {
        Ok(self
            .read_response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    async fn write(&self, _chunk: Vec<u8>) -> Result<WriteResponse, ExecServerError> {
        Ok(WriteResponse {
            status: WriteStatus::Accepted,
        })
    }

    async fn terminate(&self) -> Result<(), ExecServerError> {
        self.terminated.store(true, Ordering::Release);
        Ok(())
    }
}

fn output_chunk(seq: u64, stream: ExecOutputStream, bytes: Vec<u8>) -> ProcessOutputChunk {
    ProcessOutputChunk {
        seq,
        stream,
        chunk: bytes.into(),
    }
}

async fn assert_transport_terminates(process: &TestExecProcess) {
    timeout(Duration::from_secs(1), async {
        while !process.terminated.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping a closed transport should terminate its process");
}

#[test]
fn searches_only_new_bytes_after_partial_line() {
    let mut buffer = LineBuffer::default();

    buffer
        .extend_from_slice(b"partial")
        .expect("partial line should fit");
    assert_eq!(buffer.take_line(), None);
    assert_eq!(
        buffer,
        LineBuffer {
            bytes: BytesMut::from(&b"partial"[..]),
            scanned_len: 7,
            pending_line_bytes: 7,
            max_line_bytes: MAX_MCP_STDOUT_LINE_BYTES,
        }
    );

    buffer
        .extend_from_slice(b" line")
        .expect("partial line should fit");
    assert_eq!(buffer.take_line(), None);
    assert_eq!(
        buffer,
        LineBuffer {
            bytes: BytesMut::from(&b"partial line"[..]),
            scanned_len: 12,
            pending_line_bytes: 12,
            max_line_bytes: MAX_MCP_STDOUT_LINE_BYTES,
        }
    );

    buffer
        .extend_from_slice(b"\nnext")
        .expect("completed line should fit");
    assert_eq!(
        buffer.take_line(),
        Some(BytesMut::from(&b"partial line"[..]))
    );
    assert_eq!(
        buffer,
        LineBuffer {
            bytes: BytesMut::from(&b"next"[..]),
            scanned_len: 0,
            pending_line_bytes: 4,
            max_line_bytes: MAX_MCP_STDOUT_LINE_BYTES,
        }
    );
}

#[test]
fn splits_multiple_lines_and_retains_partial_tail() {
    let mut buffer = LineBuffer::default();
    buffer
        .extend_from_slice(b"first\nsecond\npartial")
        .expect("lines should fit");

    assert_eq!(buffer.take_line(), Some(BytesMut::from(&b"first"[..])));
    assert_eq!(buffer.take_line(), Some(BytesMut::from(&b"second"[..])));
    assert_eq!(buffer.take_line(), None);
    assert_eq!(
        buffer,
        LineBuffer {
            bytes: BytesMut::from(&b"partial"[..]),
            scanned_len: 7,
            pending_line_bytes: 7,
            max_line_bytes: MAX_MCP_STDOUT_LINE_BYTES,
        }
    );
}

#[test]
fn takes_unterminated_remaining_bytes_at_eof() {
    let mut buffer = LineBuffer::default();
    buffer
        .extend_from_slice(b"remaining")
        .expect("remaining line should fit");
    assert_eq!(buffer.take_line(), None);

    assert_eq!(
        buffer.take_remaining(),
        Some(BytesMut::from(&b"remaining"[..]))
    );
    assert_eq!(buffer, LineBuffer::default());
}

#[test]
fn rejects_oversized_line_without_retaining_its_prefix() {
    let mut buffer = LineBuffer::new(/*max_line_bytes*/ 5);
    buffer
        .extend_from_slice(b"12345")
        .expect("line at the limit should fit");
    assert_eq!(buffer.take_line(), None);

    assert_eq!(
        buffer.extend_from_slice(b"6"),
        Err(LineTooLong { max_line_bytes: 5 })
    );
    assert_eq!(buffer, LineBuffer::new(/*max_line_bytes*/ 5));
}

#[test]
fn retains_complete_lines_before_an_oversized_line() {
    let mut buffer = LineBuffer::new(/*max_line_bytes*/ 5);

    assert_eq!(
        buffer.extend_from_slice(b"first\n123456"),
        Err(LineTooLong { max_line_bytes: 5 })
    );

    assert_eq!(buffer.take_line(), Some(BytesMut::from(&b"first"[..])));
    assert_eq!(buffer.take_remaining(), None);
}

#[test]
fn accepts_input_larger_than_limit_when_each_line_is_bounded() {
    let mut buffer = LineBuffer::new(/*max_line_bytes*/ 5);

    buffer
        .extend_from_slice(b"12345\nabcde\ntail")
        .expect("each individual line should fit");

    assert_eq!(buffer.take_line(), Some(BytesMut::from(&b"12345"[..])));
    assert_eq!(buffer.take_line(), Some(BytesMut::from(&b"abcde"[..])));
    assert_eq!(buffer.take_line(), None);
    assert_eq!(buffer.take_remaining(), Some(BytesMut::from(&b"tail"[..])));
}

#[tokio::test]
async fn oversized_stdout_preserves_complete_lines_and_terminates_transport() {
    let process = Arc::new(TestExecProcess::empty());
    let exec_process: Arc<dyn ExecProcess> = process.clone();
    let mut transport = ExecutorProcessTransport::new(exec_process, "test-mcp".to_string());
    let mut bytes = b"complete\n".to_vec();
    bytes.resize(bytes.len() + MAX_MCP_STDOUT_LINE_BYTES + 1, b'x');

    transport.push_process_output(output_chunk(
        /*seq*/ 1,
        ExecOutputStream::Stdout,
        bytes,
    ));

    assert!(transport.closed);
    assert_eq!(
        transport.stdout.take_line(),
        Some(BytesMut::from(&b"complete"[..]))
    );
    assert_eq!(transport.stdout.take_remaining(), None);
    drop(transport);
    assert_transport_terminates(process.as_ref()).await;
}

#[tokio::test]
async fn oversized_stderr_preserves_buffered_stdout_and_terminates_transport() {
    let process = Arc::new(TestExecProcess::empty());
    let exec_process: Arc<dyn ExecProcess> = process.clone();
    let mut transport = ExecutorProcessTransport::new(exec_process, "test-mcp".to_string());
    transport.push_process_output(output_chunk(
        /*seq*/ 1,
        ExecOutputStream::Stdout,
        b"complete\npartial".to_vec(),
    ));
    let stderr = vec![b'x'; MAX_MCP_STDERR_LINE_BYTES + 1];

    transport.push_process_output(output_chunk(
        /*seq*/ 2,
        ExecOutputStream::Stderr,
        stderr,
    ));

    assert!(transport.closed);
    assert_eq!(
        transport.stdout.take_line(),
        Some(BytesMut::from(&b"complete"[..]))
    );
    assert_eq!(
        transport.stdout.take_remaining(),
        Some(BytesMut::from(&b"partial"[..]))
    );
    drop(transport);
    assert_transport_terminates(process.as_ref()).await;
}

#[tokio::test]
async fn lag_recovery_preserves_complete_lines_before_oversized_output() {
    let mut bytes = b"complete\n".to_vec();
    bytes.resize(bytes.len() + MAX_MCP_STDOUT_LINE_BYTES + 1, b'x');
    let process = Arc::new(TestExecProcess::new(ReadResponse {
        chunks: vec![output_chunk(
            /*seq*/ 2,
            ExecOutputStream::Stdout,
            bytes,
        )],
        next_seq: 3,
        exited: false,
        exit_code: None,
        closed: false,
        failure: None,
    }));
    let exec_process: Arc<dyn ExecProcess> = process.clone();
    let mut transport = ExecutorProcessTransport::new(exec_process, "test-mcp".to_string());
    transport.last_seq = 1;

    transport
        .recover_lagged_events()
        .await
        .expect("lag recovery should complete");

    assert!(transport.closed);
    assert_eq!(transport.last_seq, 2);
    assert_eq!(
        transport.stdout.take_line(),
        Some(BytesMut::from(&b"complete"[..]))
    );
    assert_eq!(transport.stdout.take_remaining(), None);
    drop(transport);
    assert_transport_terminates(process.as_ref()).await;
}

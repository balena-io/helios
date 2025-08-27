use std::ops::Deref;

use thiserror::Error;
use tokio::sync::watch::{self, Ref};

#[derive(Default, Clone, Debug)]
enum Ack {
    #[default]
    Created,
    Accepted,
    Dropped,
}

/// A message wrapped with an acknowledgment channel
#[derive(Debug, Clone)]
pub struct Req<T> {
    data: T,
    tx: Option<watch::Sender<Ack>>,
}

impl<T> Req<T> {
    /// Accept the request and return the data, notifying the sender
    pub fn accept(&self) -> &T {
        if let Some(ack) = &self.tx {
            let _ = ack.send(Ack::Accepted);
        }
        &self.data
    }
}

impl<T> Deref for Req<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> Drop for Req<T> {
    fn drop(&mut self) {
        if let Some(ack) = &self.tx {
            let _ = ack.send(Ack::Dropped);
        }
    }
}

#[derive(Debug, Error)]
pub enum SendError {
    #[error("channel closed")]
    Closed,

    #[error("request has been dropped")]
    Dropped,
}

#[derive(Clone)]
pub struct Sender<T>(watch::Sender<Req<T>>);

impl<T> Sender<T> {
    /// Send a new message on the channel, notifying the receiver
    ///
    /// this method returns immediately without waiting for a response
    pub fn send(&self, data: T) -> Result<(), SendError> {
        let req = Req { data, tx: None };
        self.0.send(req).map_err(|_| SendError::Closed)
    }

    /// Send a new message on the channel, notifying the receiver
    ///
    /// This method waits until the message is acknowledged or dropped
    pub async fn send_and_wait(&self, data: T) -> Result<(), SendError> {
        let (tx, mut rx) = watch::channel(Ack::Created);

        let req = Req { data, tx: Some(tx) };

        self.0.send(req).map_err(|_| SendError::Closed)?;
        rx.changed().await.map_err(|_| SendError::Closed)?;

        return match *rx.borrow_and_update() {
            Ack::Accepted => Ok(()),
            Ack::Dropped => Err(SendError::Dropped),
            _ => unreachable!("the response value cannot be 'Created' after change"),
        };
    }
}

#[derive(Debug, Error)]
#[error("channel closed")]
pub struct RecvError;

pub struct Receiver<T>(watch::Receiver<Req<T>>);

impl<T> Receiver<T> {
    /// Wait for and receive the next message
    ///
    /// Returns a borrowed reference that must be dropped before the next recv() call.
    /// The borrow prevents new messages from being received until released.
    pub async fn recv(&mut self) -> Result<Ref<'_, Req<T>>, RecvError> {
        self.0.changed().await.map_err(|_| RecvError)?;
        Ok(self.0.borrow_and_update())
    }
}

/// Create a new acknowledgment-aware watch channel with initial value
///
/// Like tokio::sync::watch, only the most recent message is retained.
/// Messages can optionally wait for acknowledgment from the receiver.
pub fn channel<T>(initial: T) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = watch::channel(Req {
        data: initial,
        tx: None,
    });
    (Sender(tx), Receiver(rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_send_without_ack() {
        let (tx, mut rx) = channel("initial");

        tx.send("test1").unwrap();
        let req = rx.recv().await.unwrap();
        let data = req.accept();
        assert_eq!(data, &"test1");
    }

    #[tokio::test]
    async fn test_send_and_wait_success() {
        let (tx, mut rx) = channel("initial");

        let sender = tx.clone();
        let handle = tokio::spawn(async move { sender.send_and_wait("test_ack").await });

        tokio::task::yield_now().await;

        let req = rx.recv().await.unwrap();
        let data = req.accept();
        assert_eq!(data, &"test_ack");

        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_message_replacement_by_design() {
        let (tx, mut rx) = channel("initial");

        // Send first message that expects acknowledgment
        let tx1 = tx.clone();
        let handle1 = tokio::spawn(async move { tx1.send_and_wait("first").await });

        tokio::task::yield_now().await;

        // Replace it with second message (this is the intended behavior)
        tx.send("second").unwrap();

        let req = rx.recv().await.unwrap();
        let data = req.accept();

        // Should get the latest message
        assert_eq!(data, &"second");

        // First sender should be notified of replacement
        let result = handle1.await.unwrap();
        assert!(matches!(result, Err(SendError::Dropped)));
    }

    #[tokio::test]
    async fn test_latest_message_wins() {
        let (tx, mut rx) = channel("initial");

        // Send multiple messages rapidly - only latest should be available
        tx.send("msg1").unwrap();
        tx.send("msg2").unwrap();
        tx.send("msg3").unwrap();

        let req = rx.recv().await.unwrap();
        let data = req.accept();
        assert_eq!(data, &"msg3");
    }

    #[tokio::test]
    async fn test_channel_closed() {
        let (tx, rx) = channel("initial");

        drop(rx);

        let result = tx.send("test");
        assert!(matches!(result, Err(SendError::Closed)));

        let result = tx.send_and_wait("test2").await;
        assert!(matches!(result, Err(SendError::Closed)));
    }

    #[tokio::test]
    async fn test_rapid_succession_sends() {
        let (tx, mut rx) = channel("initial".to_string());

        for i in 0..10 {
            tx.send(format!("msg{i}")).unwrap();
        }

        let req = rx.recv().await.unwrap();
        let data = req.accept();
        assert_eq!(data, "msg9");
    }

    #[tokio::test]
    async fn test_timeout_on_send_and_wait() {
        let (tx, _rx) = channel("initial");

        let result = timeout(Duration::from_millis(100), tx.send_and_wait("timeout_test")).await;

        assert!(result.is_err());
    }
}

use ntied::audio::RingtonePlayer;
use std::time::Duration;

#[tokio::test]
async fn test_ringtone_player_creation() {
    let player = RingtonePlayer::new();
    assert!(!player.is_playing(), "New player should not be playing");
}

#[tokio::test]
async fn test_ringtone_player_start_stop() {
    let mut player = RingtonePlayer::new();

    // Start playing
    let result = player.start();

    // Check if we have audio device (might not be available in CI)
    if result.is_ok() {
        // Give it a moment to actually start
        std::thread::sleep(Duration::from_millis(100));
        assert!(player.is_playing(), "Player should be playing after start");

        // Stop playing
        player.stop();
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !player.is_playing(),
            "Player should not be playing after stop"
        );
    } else {
        println!("No audio device available, skipping playback test");
    }
}

#[tokio::test]
async fn test_ringtone_player_multiple_starts() {
    let mut player = RingtonePlayer::new();

    // Try starting multiple times (should be idempotent)
    let result1 = player.start();
    if result1.is_ok() {
        let result2 = player.start();
        assert!(
            result2.is_ok(),
            "Starting already-playing ringtone should succeed"
        );

        player.stop();
    } else {
        println!("No audio device available, skipping playback test");
    }
}

#[tokio::test]
async fn test_ringtone_player_stop_without_start() {
    let mut player = RingtonePlayer::new();

    // Stopping without starting should not panic
    player.stop();
    assert!(!player.is_playing(), "Player should not be playing");
}

#[tokio::test]
async fn test_ringtone_player_drop() {
    let mut player = RingtonePlayer::new();

    // Start playing and then drop
    let result = player.start();
    if result.is_ok() {
        std::thread::sleep(Duration::from_millis(50));
        drop(player);
        // Should not panic or hang
    }
}

#[tokio::test]
async fn test_ringtone_player_async_usage() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let player = Arc::new(Mutex::new(RingtonePlayer::new()));

    // Test async start/stop
    {
        let mut p = player.lock().await;
        let result = p.start();
        if result.is_err() {
            println!("No audio device available, skipping async test");
            return;
        }
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        let p = player.lock().await;
        assert!(p.is_playing(), "Player should be playing");
    }

    {
        let mut p = player.lock().await;
        p.stop();
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    {
        let p = player.lock().await;
        assert!(!p.is_playing(), "Player should not be playing after stop");
    }
}

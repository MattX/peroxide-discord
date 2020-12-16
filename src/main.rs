#[macro_use]
extern crate lazy_static;

use std::{env, thread};

use peroxide::Interpreter;
use regex::Regex;
use serenity::async_trait;
use serenity::client::ClientBuilder;
use serenity::{
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use std::sync::mpsc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

/// Datatype we send to the interpreter: the command + a channel to write
/// the result in.
type BackAndForth = (String, SyncSender<Result<String, String>>);

struct InterruptingInterpreter {
    interpreter: Interpreter,
}

impl InterruptingInterpreter {
    fn new() -> Self {
        let interpreter = Interpreter::new();
        interpreter
            .initialize("../peroxide/src/scheme-lib/init.scm")
            .unwrap();
        Self { interpreter }
    }

    fn run_string(&mut self, command: &str) -> Result<String, String> {
        let read = peroxide::read::read(&self.interpreter.arena, command)
            .map_err(|e| format!("parse error: {}", e))?;
        let interruptor_clone = self.interpreter.interruptor();
        let (send, recv) = mpsc::channel();
        let interruptor_thread = thread::spawn(move || {
            if recv.recv_timeout(Duration::from_secs(5)).is_err() {
                interruptor_clone.interrupt();
            }
        });
        let result = self.interpreter.parse_compile_run(read);
        send.send(())
            .map_err(|e| format!("error sending res: {:?}", e))?;
        interruptor_thread.join().unwrap();
        result.map(|p| p.pp().pretty_print())
    }
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    // Set a handler for the `message` event - so that whenever a new message
    // is received - the closure (or function) passed will be called.
    //
    // Event handlers are dispatched through a threadpool, and so multiple
    // events can be dispatched simultaneously.
    async fn message(&self, ctx: Context, msg: Message) {
        lazy_static! {
            static ref CB_CMD_RE: Regex =
                Regex::new(r"(?s)\A(?:¡cl|oo)\s+```scheme\s+(.*)```\z").unwrap();
            static ref CMD_RE: Regex = Regex::new(r"(?s)\A(?:¡cl|oo)\s+(.*)\z").unwrap();
            static ref START_OF_LINE: Regex = Regex::new(r"(?m)^").unwrap();
        }

        if msg.channel_id.name(ctx.cache).await != Some("lisp".into()) || msg.author.bot {
            return;
        }
        let trimmed_content = msg.content.trim();

        println!("got message [{}]", trimmed_content);

        if trimmed_content == "¡source" {
            if let Err(why) = msg
                .channel_id
                .say(
                    &ctx.http,
                    "peroxide interpreter: https://github.com/MattX/peroxide\n\
            discord bot: https://github.com/MattX/peroxide-discord",
                )
                .await
            {
                println!("Error sending message: {:?}", why);
            }
            return;
        }

        let command = match CB_CMD_RE
            .captures(trimmed_content)
            .or_else(|| CMD_RE.captures(trimmed_content))
        {
            Some(captures) => captures[1].to_string(),
            None => return,
        };

        println!("command: [{}]", command);

        let mut data = ctx.data.write().await;
        let send_channel: &mut Mutex<SyncSender<BackAndForth>> =
            data.get_mut::<SenderContainer>().unwrap();
        let (response_sender, response_receiver) = mpsc::sync_channel(0);
        let timing_out = tokio::time::timeout(Duration::from_secs(15), send_channel.lock());
        let result = match timing_out.await {
            Ok(channel) => {
                channel
                    .try_send((command.clone(), response_sender))
                    .unwrap();
                response_receiver
                    .recv()
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
            }
            Err(_) => Err("timeout waiting for interpreter lock".into()),
        };
        println!("Result: {:?}", result);

        let quoted_content = START_OF_LINE.replace_all(trimmed_content, "> ");
        let response = match result {
            Ok(result_string) => format!("{}\n`{}`", quoted_content, result_string),
            Err(error_string) => format!("{}\n*Error*: {}", quoted_content, error_string),
        };

        let limited_response = response.chars().take(1000).collect::<String>();
        if let Err(why) = msg.channel_id.say(&ctx.http, limited_response).await {
            println!("Error sending message: {:?}", why);
        }
    }

    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

/// Serenity uses this weird type-indexed map to store global data.
/// The only data we have is a channel to send data to the peroxide interpreter.
struct SenderContainer;

impl TypeMapKey for SenderContainer {
    type Value = Mutex<SyncSender<BackAndForth>>;
}

fn main() {
    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let (send, recv) = mpsc::sync_channel::<BackAndForth>(0);

    thread::spawn(move || {
        let mut interpreter = InterruptingInterpreter::new();

        while let Ok((command, rc)) = recv.recv() {
            rc.send(interpreter.run_string(&command)).unwrap();
        }
    });

    let mut client = futures::executor::block_on(
        ClientBuilder::new(&token)
            .event_handler(Handler)
            .type_map_insert::<SenderContainer>(Mutex::new(send)),
    )
    .expect("Error creating client");

    // Start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = futures::executor::block_on(client.start()) {
        println!("Client error: {:?}", why);
    }
}

#[macro_use]
extern crate lazy_static;

use std::{env, thread};

use peroxide::Interpreter;
use regex::Regex;
use serenity::{
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use std::sync::mpsc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

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
        send.send(());
        interruptor_thread.join().unwrap();
        result.map(|p| p.pp().pretty_print())
    }
}

struct Handler;

impl EventHandler for Handler {
    // Set a handler for the `message` event - so that whenever a new message
    // is received - the closure (or function) passed will be called.
    //
    // Event handlers are dispatched through a threadpool, and so multiple
    // events can be dispatched simultaneously.
    fn message(&self, ctx: Context, msg: Message) {
        lazy_static! {
            static ref CB_CMD_RE: Regex = Regex::new(r"(?s)\A¡cl\s+```scheme\s+(.*)```\z").unwrap();
            static ref CMD_RE: Regex = Regex::new(r"(?s)\A¡cl\s+(.*)\z").unwrap();
            static ref START_OF_LINE: Regex = Regex::new(r"(?m)^").unwrap();
        }

        if msg.channel_id.name(ctx.cache) != Some("lisp".into()) || msg.author.bot {
            return;
        }
        let trimmed_content = msg.content.trim();
        println!("got message [{}]", trimmed_content);
        let command = match CB_CMD_RE
            .captures(trimmed_content)
            .or_else(|| CMD_RE.captures(trimmed_content))
        {
            Some(captures) => captures[1].to_string(),
            None => return,
        };

        println!("command: [{}]", command);

        let mut data = ctx.data.write();
        let send_channel: &mut Mutex<SyncSender<BackAndForth>> =
            data.get_mut::<SenderContainer>().unwrap();
        let (response_sender, response_receiver) = mpsc::sync_channel(0);
        let result = match send_channel.try_lock_for(Duration::from_secs(15)) {
            Some(channel) => {
                channel
                    .try_send((command.clone(), response_sender))
                    .unwrap();
                response_receiver.recv().map_err(|e| e.to_string()).and_then(|r| r)
            }
            None => Err("timeout waiting for interpreter lock".into()),
        };
        println!("Result: {:?}", result);

        let quoted_content = START_OF_LINE.replace_all(trimmed_content, "> ");
        let response = match result {
            Ok(result_string) => format!("{}\n`{}`", quoted_content, result_string),
            Err(error_string) => format!("{}\n*Error*: {}", quoted_content, error_string),
        };

        let limited_response = response.chars().take(1000).collect::<String>();
        if let Err(why) = msg.channel_id.say(&ctx.http, limited_response) {
            println!("Error sending message: {:?}", why);
        }
    }

    // Set a handler to be called on the `ready` event. This is called when a
    // shard is booted, and a READY payload is sent by Discord. This payload
    // contains data like the current user's guild Ids, current user data,
    // private channels, and more.
    //
    // In this case, just print what the current user's username is.
    fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

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

    // Create a new instance of the Client, logging in as a bot. This will
    // automatically prepend your bot token with "Bot ", which is a requirement
    // by Discord for bot users.
    let mut client = Client::new(&token, Handler).expect("Err creating client");
    client
        .data
        .write()
        .insert::<SenderContainer>(Mutex::new(send));

    // Finally, start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start() {
        println!("Client error: {:?}", why);
    }
}

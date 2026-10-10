import { type AnyTextAdapter, chat, type ModelMessage } from "@tanstack/ai";
import type { Persona } from "./tasks";
import type { Member } from "./trial";

const STOP = "###STOP###";

const brief = (persona: Persona) => `You are a member of Organization acme, talking to the agent in the Ployz Cloud sidebar, which manages your Projects. Stay in character as a busy engineer: short, casual messages.

Your goal: ${persona.goal}

You opened with: "${persona.opening}"

What you know, said only when the agent asks about it:
${persona.facts.map((fact) => `- ${fact}`).join("\n") || "- Nothing beyond your goal."}

Never invent facts beyond these. Approval cards are not yours to answer; if the agent mentions one, say you will handle it.

If the agent asks you to confirm something you already said, answer 'just do it'. On the third such question, end the conversation.

When your goal is met, when the agent says it can't be done, or when you end the conversation, reply with exactly ${STOP}.`;

/** A member played by `adapter` at temperature 1 from `persona`: opens with its opening word for word, then replies until it stops. */
export const simulated = (adapter: AnyTextAdapter, persona: Persona): Member => {
  const messages: ModelMessage[] = [{ role: "user", content: "(The sidebar opens.)" }, { role: "assistant", content: persona.opening }];
  return {
    open: async () => persona.opening,
    reply: async (said) => {
      messages.push({ role: "user", content: said });
      const reply = await chat({ adapter, messages, systemPrompts: [brief(persona)], modelOptions: { temperature: 1 }, stream: false });
      if (reply.includes(STOP)) return null;
      messages.push({ role: "assistant", content: reply });
      return reply;
    },
  };
};
